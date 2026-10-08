//! The file format, and compression / decompression of whole files.
//!
//! The pixel data of a PSD/PSB comes in *spans*: a layer channel's data (past its
//! compression field) or the composite image's (all planes). 8-bit spans that are RLE
//! (PackBits) or raw are taken out of the file and their planes coded by the pixel model;
//! RLE is rebuilt by the encoder identified for the span ([`packbits`]), with the rows it
//! does not reproduce stored verbatim. Files embedded in smart objects are spans too, coded
//! by the recompressor of their format (this one for PSD/PSB, png-recons, jpeg-recons) when
//! that makes them smaller. What remains of the file, the *skeleton*, is compressed with
//! zstd.
//!
//! ```text
//! "PSDR" version(u8) mode(u8)
//! mode 0 (stored): zstd(file)
//! mode 1 (coded):
//!   u64 LE xxh3 of the original (checked after decompression)
//!   varint length + zstd(skeleton)
//!   varint length + layout          -- spans, encoders, exceptions, groups
//!   model payload                   -- see model::Recorder::finish
//! ```

use crate::composite;
use crate::model::{self, Group};
use crate::packbits::{self, Encoder};
use crate::pixels::unpackbits;
use crate::psd::{self, Compression, Error, Psd, Result};

const MAGIC: &[u8; 4] = b"PSDR";
const VERSION: u8 = 1;

fn corrupt() -> Error {
    if std::env::var_os("PSDR_DEBUG").is_some() {
        eprintln!("corrupt at {}", std::panic::Location::caller());
    }
    Error("corrupt input".into())
}

pub fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push(v as u8 | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

pub fn get_varint(src: &[u8], at: &mut usize) -> Result<u64> {
    let mut v = 0u64;
    for shift in (0..64).step_by(7) {
        let b = *src.get(*at).ok_or_else(corrupt)?;
        *at += 1;
        v |= ((b & 0x7f) as u64) << shift;
        if b < 0x80 {
            return Ok(v);
        }
    }
    Err(corrupt())
}

fn get_usize(src: &[u8], at: &mut usize) -> Result<usize> {
    usize::try_from(get_varint(src, at)?).map_err(|_| corrupt())
}

fn get_bytes<'a>(src: &'a [u8], at: &mut usize, n: usize) -> Result<&'a [u8]> {
    let s = src.get(*at..at.checked_add(n).ok_or_else(corrupt)?).ok_or_else(corrupt)?;
    *at += n;
    Ok(s)
}

/// `usize::MAX` as 0, anything else shifted by one.
fn put_opt(out: &mut Vec<u8>, v: usize) {
    put_varint(out, if v == usize::MAX { 0 } else { v as u64 + 1 });
}

fn get_opt(src: &[u8], at: &mut usize) -> Result<usize> {
    Ok(match get_varint(src, at)? {
        0 => usize::MAX,
        v => usize::try_from(v - 1).map_err(|_| corrupt())?,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Codec {
    Psd,
    Png,
    Jpeg,
}

#[derive(Debug, Clone)]
enum Coding {
    Raw,
    Rle { count_bytes: usize, encoder: Encoder, exceptions: Vec<(usize, Vec<u8>)> },
    /// An embedded file, compressed by `codec`.
    Nested { codec: Codec, packed: Vec<u8> },
}

/// How deep embedded PSDs are recompressed.
const MAX_DEPTH: usize = 3;

fn nested_compress(codec: Codec, data: &[u8], depth: usize) -> Option<Vec<u8>> {
    let packed = match codec {
        Codec::Psd => try_compress(data, depth + 1)?,
        Codec::Png => png_recons::compress(data),
        Codec::Jpeg => jpeg_recons::container::compress(data),
    };
    (packed.len() < data.len()).then_some(packed)
}

fn nested_decompress(codec: Codec, packed: &[u8]) -> Result<Vec<u8>> {
    match codec {
        Codec::Psd => decompress(packed),
        Codec::Png => png_recons::decompress(packed).map_err(|_| corrupt()),
        Codec::Jpeg => jpeg_recons::container::decompress(packed).map_err(|_| corrupt()),
    }
}

/// A span of pixel data taken out of the file: `planes` planes of `h` rows of `w` bytes.
/// `gap` is the number of skeleton bytes before it (after the previous span).
#[derive(Debug, Clone)]
struct Span {
    start: usize,
    end: usize,
    gap: usize,
    planes: usize,
    w: usize,
    h: usize,
    coding: Coding,
}

/// A plane of a span: (span index, plane index).
type PlaneRef = (usize, usize);

#[derive(Debug, Clone)]
struct GroupDef {
    planes: Vec<PlaneRef>,
    has_alpha: bool,
    /// Coded with the composite rendered from the layers as hint (all planes in one span,
    /// which is the composite's).
    hinted: bool,
}

/// Rebuilds a span's bytes from its decoded planes (planar, `planes * h * w` bytes).
fn rebuild(s: &Span, data: &[u8], out: &mut Vec<u8>) -> Result<()> {
    match &s.coding {
        Coding::Raw => out.extend_from_slice(data),
        Coding::Nested { codec, packed } => out.extend_from_slice(&nested_decompress(*codec, packed)?),
        Coding::Rle { count_bytes, encoder, exceptions } => {
            let rows = s.planes * s.h;
            let counts_at = out.len();
            out.resize(counts_at + rows * count_bytes, 0);
            let mut ex = exceptions.iter().peekable();
            for r in 0..rows {
                let before = out.len();
                match ex.peek() {
                    Some((i, bytes)) if *i == r => {
                        out.extend_from_slice(bytes);
                        ex.next();
                    }
                    _ => packbits::encode(*encoder, &data[r * s.w..(r + 1) * s.w], out),
                }
                let n = out.len() - before;
                let c = &mut out[counts_at + r * count_bytes..counts_at + (r + 1) * count_bytes];
                for (k, b) in c.iter_mut().rev().enumerate() {
                    *b = (n >> (8 * k)) as u8;
                }
            }
        }
    }
    Ok(())
}

/// Decodes a candidate span and works out how to rebuild it, or None if it cannot be.
fn analyze_span(d: &[u8], start: usize, end: usize, planes: usize, w: usize, h: usize, comp: Compression, psb: bool) -> Option<(Span, Vec<u8>)> {
    let src = &d[start..end];
    let rows = planes * h;
    let (coding, data) = match comp {
        Compression::Raw => {
            if src.len() != rows * w {
                return None;
            }
            (Coding::Raw, src.to_vec())
        }
        Compression::Rle => {
            let cb = if psb { 4 } else { 2 };
            let enc_rows = packbits::rows(src, rows, cb)?;
            let mut data = Vec::with_capacity(rows * w);
            for r in &enc_rows {
                let before = data.len();
                if !unpackbits(r, &mut data) || data.len() - before != w {
                    return None;
                }
            }
            // The counts must be what rebuilding writes: the rows' lengths, and no slack.
            let used = rows * cb + enc_rows.iter().map(|r| r.len()).sum::<usize>();
            if used != src.len() || enc_rows.iter().any(|r| r.len() >> (8 * cb).min(63) != 0) {
                return None;
            }
            let decoded: Vec<&[u8]> = data.chunks(w).collect();
            // One encoder per plane would fit better; one per span is what the format has.
            let (encoder, bad) = packbits::identify_rows(&enc_rows, &decoded);
            let exceptions = bad.into_iter().map(|i| (i, enc_rows[i].to_vec())).collect();
            (Coding::Rle { count_bytes: cb, encoder, exceptions }, data)
        }
        _ => return None,
    };
    let span = Span { start, end, gap: 0, planes, w, h, coding };
    if cfg!(debug_assertions) {
        let mut back = Vec::with_capacity(src.len());
        rebuild(&span, &data, &mut back).ok()?;
        assert!(back == src, "span at {start} does not rebuild");
    }
    Some((span, data))
}

/// Spans and groups of a parsed file, with the decoded planes of each span.
fn plan(psd: &Psd, d: &[u8], depth: usize) -> (Vec<Span>, Vec<Vec<u8>>, Vec<GroupDef>) {
    let mut spans = Vec::new();
    let mut datas = Vec::new();
    let mut groups = Vec::new();
    let pixels = psd.depth == 8;
    let rgb = psd.mode == 3;
    for l in psd.layer_info.layers.iter().filter(|_| pixels) {
        let mut refs: Vec<(i16, PlaneRef)> = Vec::new();
        for c in &l.channels {
            let (w, h) = (c.width as usize, c.height as usize);
            let Some(comp) = c.compression(d) else { continue };
            if w == 0 || h == 0 {
                continue;
            }
            if let Some((s, data)) = analyze_span(d, c.span.start + 2, c.span.end, 1, w, h, comp, psd.psb) {
                refs.push((c.id, (spans.len(), 0)));
                spans.push(s);
                datas.push(data);
            }
        }
        let pick = |id: i16| refs.iter().find(|r| r.0 == id).map(|r| r.1);
        let mut grouped = Vec::new();
        if rgb {
            if let (Some(r), Some(g), Some(b)) = (pick(0), pick(1), pick(2)) {
                let mut planes = vec![r, g, b];
                let alpha = pick(-1).filter(|a| spans[a.0].w == spans[r.0].w && spans[a.0].h == spans[r.0].h);
                if let Some(a) = alpha {
                    planes.push(a);
                }
                grouped.extend(&planes);
                groups.push(GroupDef { planes, has_alpha: alpha.is_some(), hinted: false });
            }
        }
        for (_, p) in refs {
            if !grouped.contains(&p) {
                groups.push(GroupDef { planes: vec![p], has_alpha: false, hinted: false });
            }
        }
    }
    // Embedded files, which come after the layers' pixel data.
    if depth < MAX_DEPTH {
        for f in psd::linked(psd, d) {
            if spans.last().is_some_and(|s: &Span| s.end > f.data.start) {
                continue;
            }
            let codec = match &f.file_type {
                b"8BPB" | b"8BPS" => Codec::Psd,
                b"png " | b"PNGf" => Codec::Png,
                b"JPEG" => Codec::Jpeg,
                _ => continue,
            };
            if let Some(packed) = nested_compress(codec, &d[f.data.clone()], depth) {
                let coding = Coding::Nested { codec, packed };
                spans.push(Span { start: f.data.start, end: f.data.end, gap: 0, planes: 0, w: 0, h: 0, coding });
                datas.push(Vec::new());
            }
        }
    }
    // Composite.
    let (w, h, n) = (psd.width as usize, psd.height as usize, psd.channels as usize);
    let comp = psd.composite_compression(d);
    if pixels && w > 0 && h > 0 && n > 0 {
        if let Some((s, data)) = analyze_span(d, psd.composite.start + 2, psd.composite.end, n, w, h, comp, psd.psb) {
            let si = spans.len();
            spans.push(s);
            datas.push(data);
            let mut rest = 0;
            if rgb && n >= 3 {
                let has_alpha = n >= 4 && psd.layer_info.merged_alpha;
                let k = 3 + has_alpha as usize;
                let hinted = !psd.layer_info.layers.is_empty();
                groups.push(GroupDef { planes: (0..k).map(|p| (si, p)).collect(), has_alpha, hinted });
                rest = k;
            }
            for p in rest..n {
                groups.push(GroupDef { planes: vec![(si, p)], has_alpha: false, hinted: false });
            }
        }
    }
    (spans, datas, groups)
}

fn put_encoder(out: &mut Vec<u8>, e: Encoder) {
    match e {
        Encoder::Photoshop { phase, period } => {
            out.push(0);
            put_opt(out, phase);
            put_opt(out, period);
        }
        Encoder::Greedy { min_run, split_rest } => {
            out.push(1 + split_rest as u8);
            put_varint(out, min_run as u64);
        }
    }
}

fn get_encoder(src: &[u8], at: &mut usize) -> Result<Encoder> {
    let tag = *get_bytes(src, at, 1)?.first().unwrap();
    Ok(match tag {
        0 => {
            let phase = get_opt(src, at)?;
            let period = get_opt(src, at)?;
            if period == 0 {
                return Err(corrupt());
            }
            Encoder::Photoshop { phase, period }
        }
        1 | 2 => Encoder::Greedy { min_run: get_usize(src, at)?.max(1), split_rest: tag == 2 },
        _ => return Err(corrupt()),
    })
}

fn put_layout(spans: &[Span], groups: &[GroupDef]) -> Vec<u8> {
    let mut o = Vec::new();
    put_varint(&mut o, spans.len() as u64);
    for s in spans {
        put_varint(&mut o, s.gap as u64);
        put_varint(&mut o, s.planes as u64);
        put_varint(&mut o, s.w as u64);
        put_varint(&mut o, s.h as u64);
        match &s.coding {
            Coding::Raw => o.push(0),
            Coding::Nested { codec, packed } => {
                o.push(16 + *codec as u8);
                put_varint(&mut o, packed.len() as u64);
                o.extend_from_slice(packed);
            }
            Coding::Rle { count_bytes, encoder, exceptions } => {
                o.push(*count_bytes as u8);
                put_encoder(&mut o, *encoder);
                put_varint(&mut o, exceptions.len() as u64);
                let mut pr = 0;
                for (r, bytes) in exceptions {
                    put_varint(&mut o, (r - pr) as u64);
                    pr = *r;
                    put_varint(&mut o, bytes.len() as u64);
                    o.extend_from_slice(bytes);
                }
            }
        }
    }
    put_varint(&mut o, groups.len() as u64);
    for g in groups {
        put_varint(&mut o, g.planes.len() as u64);
        o.push(g.has_alpha as u8 | (g.hinted as u8) << 1);
        for &(s, p) in &g.planes {
            put_varint(&mut o, s as u64);
            put_varint(&mut o, p as u64);
        }
    }
    o
}

fn get_layout(src: &[u8]) -> Result<(Vec<Span>, Vec<GroupDef>)> {
    let mut at = 0;
    let n = get_usize(src, &mut at)?;
    let mut spans = Vec::with_capacity(n.min(1 << 20));
    for _ in 0..n {
        let gap = get_usize(src, &mut at)?;
        let planes = get_usize(src, &mut at)?;
        let w = get_usize(src, &mut at)?;
        let h = get_usize(src, &mut at)?;
        let cb = *get_bytes(src, &mut at, 1)?.first().unwrap() as usize;
        let coding = match cb {
            0 => Coding::Raw,
            2 | 4 => {
                let encoder = get_encoder(src, &mut at)?;
                let ne = get_usize(src, &mut at)?;
                let mut exceptions = Vec::with_capacity(ne.min(1 << 16));
                let mut r = 0usize;
                for _ in 0..ne {
                    r = r.checked_add(get_usize(src, &mut at)?).ok_or_else(corrupt)?;
                    let len = get_usize(src, &mut at)?;
                    exceptions.push((r, get_bytes(src, &mut at, len)?.to_vec()));
                }
                Coding::Rle { count_bytes: cb, encoder, exceptions }
            }
            16..=18 => {
                let codec = [Codec::Psd, Codec::Png, Codec::Jpeg][cb - 16];
                let len = get_usize(src, &mut at)?;
                Coding::Nested { codec, packed: get_bytes(src, &mut at, len)?.to_vec() }
            }
            _ => return Err(corrupt()),
        };
        // Positions are only known once rebuilt.
        spans.push(Span { start: 0, end: 0, gap, planes, w, h, coding });
    }
    let ng = get_usize(src, &mut at)?;
    let mut groups = Vec::with_capacity(ng.min(1 << 20));
    for _ in 0..ng {
        let np = get_usize(src, &mut at)?;
        let flags = *get_bytes(src, &mut at, 1)?.first().unwrap();
        let (has_alpha, hinted) = (flags & 1 != 0, flags & 2 != 0);
        if flags > 3 {
            return Err(corrupt());
        }
        let mut planes = Vec::with_capacity(np.min(16));
        for _ in 0..np {
            let s = get_usize(src, &mut at)?;
            let p = get_usize(src, &mut at)?;
            if s >= spans.len() || p >= spans[s].planes {
                return Err(corrupt());
            }
            planes.push((s, p));
        }
        if np == 0 || has_alpha && np < 2 || hinted && (np < 3 || planes.iter().any(|p| p.0 != planes[0].0)) {
            return Err(corrupt());
        }
        groups.push(GroupDef { planes, has_alpha, hinted });
    }
    if at != src.len() {
        return Err(corrupt());
    }
    Ok((spans, groups))
}

fn group_of(def: &GroupDef, spans: &[Span], datas: Option<&[Vec<u8>]>) -> Result<Group> {
    let (s0, _) = def.planes[0];
    let (w, h) = (spans[s0].w, spans[s0].h);
    let mut planes = Vec::with_capacity(def.planes.len());
    for &(s, p) in &def.planes {
        if spans[s].w != w || spans[s].h != h {
            return Err(corrupt());
        }
        let n = w.checked_mul(h).ok_or_else(corrupt)?;
        planes.push(match datas {
            Some(ds) => ds[s][p * n..(p + 1) * n].to_vec(),
            None => vec![0u8; n],
        });
    }
    Ok(Group { w, h, planes, has_alpha: def.has_alpha, hint: None })
}

/// The composite of `d` (a whole file, or one up to its composite data) rendered from its
/// layers, planar, as hint for group `g`.
fn hint(d: &[u8], g: &GroupDef) -> Result<Vec<Vec<u8>>> {
    let psd = psd::parse(d)?;
    let mut rep = composite::Report::default();
    let r = composite::render(&psd, d, g.has_alpha, &mut rep)?;
    let n = g.planes.len();
    let mut planes = vec![Vec::with_capacity(r.len() / n); n];
    for px in r.chunks_exact(n) {
        for (k, &v) in px.iter().enumerate() {
            planes[k].push(v);
        }
    }
    Ok(planes)
}

/// Compresses a file; never fails (a file that cannot be coded is stored).
pub fn compress(file: &[u8]) -> Vec<u8> {
    match try_compress(file, 0) {
        Some(out) => out,
        None => {
            let mut out = Vec::with_capacity(6 + file.len() / 2);
            out.extend_from_slice(MAGIC);
            out.extend_from_slice(&[VERSION, 0]);
            out.extend_from_slice(&zstd::bulk::compress(file, 9).expect("zstd"));
            out
        }
    }
}

fn skeleton_level(n: usize) -> i32 {
    if n < 16 << 20 { 19 } else { 9 }
}

fn try_compress(file: &[u8], depth: usize) -> Option<Vec<u8>> {
    let t0 = std::time::Instant::now();
    let psd = psd::parse(file).ok()?;
    let (mut spans, datas, groups) = plan(&psd, file, depth);
    let t_plan = t0.elapsed().as_secs_f64();
    if spans.is_empty() {
        return None;
    }
    let mut sk = Vec::with_capacity(file.len() / 4);
    let mut prev = 0;
    for s in &mut spans {
        // Spans come in file order.
        if s.start < prev {
            return None;
        }
        s.gap = s.start - prev;
        sk.extend_from_slice(&file[prev..s.start]);
        prev = s.end;
    }
    sk.extend_from_slice(&file[prev..]);
    let t0 = std::time::Instant::now();
    let mut rec = model::Recorder::default();
    for g in &groups {
        let mut grp = group_of(g, &spans, Some(&datas)).ok()?;
        if g.hinted {
            grp.hint = Some(hint(file, g).ok()?);
        }
        model::walk(&mut rec, &mut grp);
    }
    let t_walk = t0.elapsed().as_secs_f64();
    let t0 = std::time::Instant::now();
    let mut out = Vec::new();
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&[VERSION, 1]);
    out.extend_from_slice(&xxhash_rust::xxh3::xxh3_64(file).to_le_bytes());
    let z = zstd::bulk::compress(&sk, skeleton_level(sk.len())).expect("zstd");
    put_varint(&mut out, z.len() as u64);
    out.extend_from_slice(&z);
    let layout = put_layout(&spans, &groups);
    put_varint(&mut out, layout.len() as u64);
    out.extend_from_slice(&layout);
    let model = rec.finish();
    if std::env::var_os("PSDR_STATS").is_some() {
        eprintln!(
            "skeleton {} -> {}, layout {}, pixels {} -> {}; plan {t_plan:.3}s walk {t_walk:.3}s zstd+rans {:.3}s",
            sk.len(),
            z.len(),
            layout.len(),
            datas.iter().map(|d| d.len()).sum::<usize>(),
            model.len(),
            t0.elapsed().as_secs_f64()
        );
    }
    out.extend_from_slice(&model);
    Some(out)
}

/// Builds the output from the skeleton and the rebuilt spans, in order.
struct Assembler<'a> {
    out: Vec<u8>,
    sk: &'a [u8],
    /// Skeleton bytes used.
    k: usize,
    /// Spans written.
    done: usize,
    /// Whether the skeleton gap before the next span is written.
    gap_done: bool,
}

impl Assembler<'_> {
    fn gap(&mut self, spans: &[Span]) -> Result<()> {
        if !self.gap_done && self.done < spans.len() {
            let end = self.k.checked_add(spans[self.done].gap).ok_or_else(corrupt)?;
            self.out.extend_from_slice(self.sk.get(self.k..end).ok_or_else(corrupt)?);
            self.k = end;
            self.gap_done = true;
        }
        Ok(())
    }

    /// Writes spans up to `upto` (excluded), with the gaps before them.
    fn spans(&mut self, spans: &[Span], datas: &[Vec<u8>], upto: usize) -> Result<()> {
        while self.done < upto {
            self.gap(spans)?;
            rebuild(&spans[self.done], &datas[self.done], &mut self.out)?;
            self.done += 1;
            self.gap_done = false;
        }
        Ok(())
    }
}

pub fn decompress(src: &[u8]) -> Result<Vec<u8>> {
    if src.len() < 6 || &src[..4] != MAGIC || src[4] != VERSION {
        return Err(Error("not a psd-recons file".into()));
    }
    let mut at = 6;
    match src[5] {
        0 => return zstd::stream::decode_all(&src[6..]).map_err(|_| corrupt()),
        1 => {}
        _ => return Err(corrupt()),
    }
    let hash = u64::from_le_bytes(get_bytes(src, &mut at, 8)?.try_into().unwrap());
    let n = get_usize(src, &mut at)?;
    let sk = zstd::stream::decode_all(get_bytes(src, &mut at, n)?).map_err(|_| corrupt())?;
    let n = get_usize(src, &mut at)?;
    let (spans, groups) = get_layout(get_bytes(src, &mut at, n)?)?;
    // Decode the planes of every group into their spans.
    let mut datas: Vec<Vec<u8>> = Vec::with_capacity(spans.len());
    for s in &spans {
        let n = s.planes.checked_mul(s.w).and_then(|v| v.checked_mul(s.h)).ok_or_else(corrupt)?;
        if n > 1 << 34 {
            return Err(corrupt());
        }
        datas.push(vec![0u8; n]);
    }
    let mut player = model::Player::new(&src[at..])?;
    // The output is assembled as soon as the spans are decoded, since a hinted group needs
    // the file up to its span.
    let mut asm = Assembler { out: Vec::with_capacity(sk.len() * 2), sk: &sk, k: 0, done: 0, gap_done: false };
    for g in &groups {
        let mut grp = group_of(g, &spans, None)?;
        if g.hinted {
            let si = g.planes[0].0;
            asm.spans(&spans, &datas, si)?;
            asm.gap(&spans)?;
            grp.hint = Some(hint(&asm.out, g)?);
        }
        model::walk(&mut player, &mut grp);
        let n = grp.w * grp.h;
        for (&(s, p), plane) in g.planes.iter().zip(grp.planes) {
            datas[s][p * n..(p + 1) * n].copy_from_slice(&plane);
        }
    }
    if !player.finished() {
        return Err(corrupt());
    }
    asm.spans(&spans, &datas, spans.len())?;
    let mut out = asm.out;
    out.extend_from_slice(sk.get(asm.k..).ok_or_else(corrupt)?);
    if xxhash_rust::xxh3::xxh3_64(&out) != hash {
        return Err(Error("checksum mismatch".into()));
    }
    Ok(out)
}
