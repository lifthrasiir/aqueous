//! Container format (specified in FORMAT.md) and the top-level compress / decompress entry points.
//!
//! ```text
//! magic 69 04 7E 8E 9D E0, u16 LE version (0 = sequential, 1 = progressive, 2 = stored)
//! then streams, each a LEB128 length and that many bytes; what each stream is follows from
//! the version and the streams before it:
//! stored:        [input]
//! sequential /
//! progressive:   [head: xxh3 of the input, embedded JPEG entries]
//!                [nested container] per embedded JPEG coded here (not a back reference)
//!                [cut blob]
//!                [scan info: per scan flags / RST count / truncation point, stripe count]
//!                [raw scan data] per raw scan, [truncated tail] per truncated scan
//!                [model tables]
//!                [rANS words] [raw bits] per stripe
//! ```
//! The blob is the input with every scan's entropy-coded data removed (headers, inter-scan
//! markers, trailer); it parses as a JPEG whose scans are empty. JPEGs embedded in it (Exif and
//! Photoshop thumbnails, MPF / vendor secondary images in the trailer, ...) are compressed
//! recursively. Decompression restores the blob, re-emits each scan from the decoded
//! coefficients and splices it back in.
//!
//! `compress` never fails: anything it cannot reproduce bit-exactly (unsupported or non-standard
//! coding, corrupt data, internal errors) is stored.

use crate::fast::{self, get_bytes, get_varint};
use crate::jpeg::{self, Clean, Coefs, Error, Order, ScanInfo};
use crate::progressive;
use recons_common::varint;
use std::borrow::Cow;
use std::time::Instant;

const MAGIC: [u8; 6] = [0x69, 0x04, 0x7E, 0x8E, 0x9D, 0xE0];
const V_SEQUENTIAL: u16 = 0;
const V_PROGRESSIVE: u16 = 1;
const V_STORED: u16 = 2;
/// scan flags (progressive scans use only the first two)
const F_PAD_ZERO: u8 = 1;
const F_RAW: u8 = 2;
const F_TRUNCATED: u8 = 4;
const F_BOUNDARY: u8 = 8;
/// nesting limit for embedded JPEGs (main image = depth 0)
const MAX_DEPTH: u32 = 3;
/// embedded JPEGs smaller than this are left alone (model tables alone would outweigh them)
const MIN_EMBEDDED: usize = 1024;

/// Wall-clock breakdown of the last call (seconds), main image only.
#[derive(Default, Clone, Copy, Debug)]
pub struct Timing {
    pub parse: f64,
    pub huffman: f64,
    pub model: f64,
}

/// Why the input (or some of its scans) was stored instead of coded (None if fully coded).
pub type StoreReason = Option<String>;

fn header(out: &mut Vec<u8>, version: u16) {
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&version.to_le_bytes());
}

fn put_varint(out: &mut Vec<u8>, v: u64) {
    varint::put(out, v)
}

fn put_stream(out: &mut Vec<u8>, d: &[u8]) {
    put_varint(out, d.len() as u64);
    out.extend_from_slice(d);
}

fn stored_len(n: usize) -> usize {
    MAGIC.len() + 2 + varint::len(n as u64) + n
}

fn stored(input: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(stored_len(input.len()));
    header(&mut out, V_STORED);
    put_stream(&mut out, input);
    out
}

/// The streams of a container, in order.
struct Streams<'a> {
    d: &'a [u8],
    p: usize,
}
impl<'a> Streams<'a> {
    fn next(&mut self) -> Result<&'a [u8], Error> {
        let n = get_varint(self.d, &mut self.p)?;
        get_bytes(self.d, &mut self.p, n)
    }
    /// The container must end with the last stream.
    fn finish(&self) -> Result<(), Error> {
        if self.p != self.d.len() { Err(Error("trailing data".into())) } else { Ok(()) }
    }
}

/// A stream's fields must use it up exactly.
fn fully_read(d: &[u8], p: usize, what: &str) -> Result<(), Error> {
    if p != d.len() { Err(Error(format!("excess data in {what} stream"))) } else { Ok(()) }
}

thread_local! {
    /// coefficient and destuffing buffers, reused across calls
    static BUFS: std::cell::RefCell<(Vec<Coefs>, Clean)> = std::cell::RefCell::new((Vec::new(), Clean::default()));
}

fn with_bufs<R>(f: impl FnOnce(&mut Vec<Coefs>, &mut Clean) -> R) -> R {
    let (mut coefs, mut clean) = BUFS.with(|b| std::mem::take(&mut *b.borrow_mut()));
    let r = f(&mut coefs, &mut clean);
    BUFS.with(|b| *b.borrow_mut() = (coefs, clean));
    r
}

// ---------------------------------------------------------------- embedded JPEGs

/// End (exclusive) of a JPEG starting with SOI at `p`, following its marker segments and
/// entropy-coded data up to EOI.
fn jpeg_extent(d: &[u8], p: usize) -> Option<usize> {
    let mut i = p + 2;
    loop {
        if *d.get(i)? != 0xFF {
            return None;
        }
        while *d.get(i + 1)? == 0xFF {
            i += 1;
        }
        let m = d[i + 1];
        match m {
            0xD9 => return Some(i + 2),
            0x00 | 0xD8 => return None,
            0x01 | 0xD0..=0xD7 => i += 2,
            _ => {
                let len = ((*d.get(i + 2)? as usize) << 8) | *d.get(i + 3)? as usize;
                if len < 2 {
                    return None;
                }
                i += 2 + len;
                if m == 0xDA {
                    if i > d.len() {
                        return None;
                    }
                    i = jpeg::scan_end(d, i);
                }
            }
        }
    }
}

/// Non-overlapping embedded JPEGs in `blob` (excluding the outer SOI at 0).
fn find_embedded(blob: &[u8]) -> Vec<(usize, usize)> {
    let mut v = Vec::new();
    let mut i = 1;
    while let Some(o) = memchr::memmem::find(&blob[i..], b"\xFF\xD8\xFF") {
        let p = i + o;
        match jpeg_extent(blob, p) {
            Some(e) if e - p >= MIN_EMBEDDED => {
                v.push((p, e));
                i = e;
            }
            _ => i = p + 3,
        }
    }
    v
}

// ---------------------------------------------------------------- compression

/// Main-image part of a coded container: blob, scan info stream, per-scan streams, model.
struct Core<'a> {
    progressive: bool,
    blob: Vec<u8>,
    info: Vec<u8>,
    /// raw scan data / truncated tails, in scan order
    scans: Vec<Cow<'a, [u8]>>,
    model: fast::Encoded,
    nraw: usize,
}

fn compress_core<'a>(
    input: &'a [u8],
    t: &mut Timing,
    coefs: &mut Vec<Coefs>,
    clean: &mut Clean,
) -> Result<Core<'a>, Error> {
    let t0 = Instant::now();
    let j = jpeg::parse(input)?;
    let t1 = Instant::now();
    let infos = if j.progressive {
        progressive::decode_all(&j, input, coefs, clean)?
    } else {
        jpeg::decode_all(&j, input, Order::Zigzag, coefs, clean)?
    };
    if infos.iter().all(|i| i.raw) {
        return Err(Error("no scan could be modelled".into()));
    }
    let t2 = Instant::now();
    let (model, _) = fast::encode(&j, coefs);
    let t3 = Instant::now();
    // blob: everything but the entropy-coded data
    let removed: usize = j.scans.iter().map(|s| s.end - s.start).sum();
    let mut blob = Vec::with_capacity(input.len() - removed);
    let mut p = 0;
    for s in &j.scans {
        blob.extend_from_slice(&input[p..s.start]);
        p = s.end;
    }
    blob.extend_from_slice(&input[p..]);
    let mut info = Vec::with_capacity(j.scans.len() * 2 + 4);
    let mut scans = Vec::new();
    let mut nraw = 0;
    put_varint(&mut info, j.scans.len() as u64);
    for (s, i) in j.scans.iter().zip(infos) {
        if i.raw {
            nraw += 1;
            info.push(F_RAW);
            scans.push(Cow::Borrowed(&input[s.start..s.end]));
            continue;
        }
        let t = i.truncated;
        info.push(
            if i.pad_zero { F_PAD_ZERO } else { 0 }
                | if t.is_some() { F_TRUNCATED } else { 0 }
                | if t.as_ref().is_some_and(|t| t.boundary) { F_BOUNDARY } else { 0 },
        );
        put_varint(&mut info, i.trailing_rst as u64);
        if let Some(t) = t {
            put_varint(&mut info, t.mcus as u64);
            put_varint(&mut info, t.bits as u64);
            scans.push(Cow::Owned(t.tail));
        }
    }
    put_varint(&mut info, model.stripes.len() as u64);
    *t = Timing { parse: (t1 - t0).as_secs_f64(), huffman: (t2 - t1).as_secs_f64(), model: (t3 - t2).as_secs_f64() };
    Ok(Core { progressive: j.progressive, blob, info, scans, model, nraw })
}

fn try_compress(input: &[u8], t: &mut Timing, depth: u32) -> Result<(Vec<u8>, usize), Error> {
    let core = with_bufs(|coefs, clean| compress_core(input, t, coefs, clean))?;
    // embedded JPEGs in the blob, compressed recursively (only where it pays off)
    let mut taken: Vec<(usize, usize)> = Vec::new();
    let mut entries = Vec::new();
    if depth < MAX_DEPTH {
        for (a, b) in find_embedded(&core.blob) {
            let bytes = &core.blob[a..b];
            if let Some(k) = taken.iter().position(|&(x, y)| core.blob[x..y] == *bytes) {
                taken.push((a, b));
                entries.push((a, b, None, k));
                continue;
            }
            if let Ok((c, _)) = try_compress(bytes, &mut Timing::default(), depth + 1)
                && c.len() + 8 < bytes.len()
            {
                taken.push((a, b));
                entries.push((a, b, Some(c), 0));
            }
        }
    }
    let mut out = Vec::with_capacity(core.blob.len() + core.info.len() + core.model.size() + 64);
    header(&mut out, if core.progressive { V_PROGRESSIVE } else { V_SEQUENTIAL });
    let mut head = Vec::with_capacity(16 + entries.len() * 8);
    head.extend_from_slice(&xxhash_rust::xxh3::xxh3_64(input).to_le_bytes());
    put_varint(&mut head, entries.len() as u64);
    let mut prev = 0;
    for (a, b, c, k) in &entries {
        put_varint(&mut head, (a - prev) as u64);
        put_varint(&mut head, (b - a) as u64);
        match c {
            Some(_) => head.push(0),
            None => {
                head.push(1);
                put_varint(&mut head, *k as u64);
            }
        }
        prev = *b;
    }
    put_stream(&mut out, &head);
    for (_, _, c, _) in &entries {
        if let Some(c) = c {
            put_stream(&mut out, c);
        }
    }
    let cut_len = core.blob.len() - entries.iter().map(|(a, b, _, _)| b - a).sum::<usize>();
    put_varint(&mut out, cut_len as u64);
    let mut p = 0;
    for (a, b, _, _) in &entries {
        out.extend_from_slice(&core.blob[p..*a]);
        p = *b;
    }
    out.extend_from_slice(&core.blob[p..]);
    put_stream(&mut out, &core.info);
    for s in &core.scans {
        put_stream(&mut out, s);
    }
    put_stream(&mut out, &core.model.tables);
    for s in &core.model.stripes {
        put_stream(&mut out, &s.rans_bytes());
        put_stream(&mut out, &s.raw);
    }
    Ok((out, core.nraw))
}

/// Compress; never fails (falls back to storing the input). Also returns why it stored.
pub fn compress_with(input: &[u8], t: &mut Timing) -> (Vec<u8>, StoreReason) {
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| try_compress(input, t, 0)));
    match r {
        Ok(Ok((v, nraw))) if v.len() < stored_len(input.len()) => {
            (v, (nraw > 0).then(|| format!("partial: {nraw} raw scan(s)")))
        }
        Ok(Ok(_)) => (stored(input), Some("no gain".into())),
        Ok(Err(e)) => (stored(input), Some(e.0)),
        Err(_) => (stored(input), Some("internal panic".into())),
    }
}

pub fn compress(input: &[u8]) -> Vec<u8> {
    compress_with(input, &mut Timing::default()).0
}

// ---------------------------------------------------------------- decompression

fn decompress_core(
    blob: &[u8],
    progressive: bool,
    r: &mut Streams,
    t: &mut Timing,
    coefs: &mut Vec<Coefs>,
) -> Result<Vec<u8>, Error> {
    let t0 = Instant::now();
    let j = jpeg::parse(blob)?;
    if j.progressive != progressive {
        return Err(Error("version does not match the frame type".into()));
    }
    let info = r.next()?;
    let mut p = 0;
    let ns = get_varint(info, &mut p)?;
    if ns != j.scans.len() as u64 {
        return Err(Error("scan count mismatch".into()));
    }
    let mut infos = Vec::with_capacity(j.scans.len());
    let mut raws = Vec::with_capacity(j.scans.len());
    let mut tails = Vec::new(); // (scan, mcus, bits, boundary)
    for k in 0..j.scans.len() {
        let f = *info.get(p).ok_or(Error("truncated".into()))?;
        p += 1;
        let known = if progressive { F_PAD_ZERO | F_RAW } else { F_PAD_ZERO | F_RAW | F_TRUNCATED | F_BOUNDARY };
        if f & !known != 0 || (f & F_RAW != 0 && f != F_RAW) || (f & F_BOUNDARY != 0 && f & F_TRUNCATED == 0) {
            return Err(Error("bad scan flags".into()));
        }
        if f & F_RAW != 0 {
            raws.push(true);
            infos.push(ScanInfo { raw: true, ..Default::default() });
            continue;
        }
        raws.push(false);
        let trailing_rst = u32::try_from(get_varint(info, &mut p)?).map_err(|_| Error("bad RST count".into()))?;
        if f & F_TRUNCATED != 0 {
            let mcus = get_varint(info, &mut p)? as usize;
            let bits = get_varint(info, &mut p)? as usize;
            tails.push((k, mcus, bits, f & F_BOUNDARY != 0));
        }
        infos.push(ScanInfo {
            standard: true,
            trailing_rst,
            pad_zero: f & F_PAD_ZERO != 0,
            truncated: None,
            raw: false,
        });
    }
    let nstripes = get_varint(info, &mut p)?;
    fully_read(info, p, "scan info")?;
    if nstripes == 0 || nstripes > fast::MAX_STRIPES as u64 {
        return Err(Error("bad stripe count".into()));
    }
    // raw scan data and truncated tails, in scan order
    let mut raw_data: Vec<Option<&[u8]>> = vec![None; j.scans.len()];
    let mut tails = tails.into_iter().peekable();
    for k in 0..j.scans.len() {
        if raws[k] {
            raw_data[k] = Some(r.next()?);
        } else if let Some(&(_, mcus, bits, boundary)) = tails.peek().filter(|x| x.0 == k) {
            tails.next();
            let tail = r.next()?.to_vec();
            infos[k].truncated = Some(jpeg::Truncation { mcus, bits, tail, boundary });
        }
    }
    let tables = r.next()?;
    let stripes: Vec<(&[u8], &[u8])> =
        (0..nstripes).map(|_| Ok((r.next()?, r.next()?))).collect::<Result<_, Error>>()?;
    r.finish()?;
    let e = fast::Encoded::from_parts(tables, &stripes)?;
    let t1 = Instant::now();
    fast::decode_into(&j, &e, coefs)?;
    let t2 = Instant::now();
    // scans are re-emitted independently of each other (in parallel; sequential scans are also
    // split internally, see `jpeg::encode_scan`)
    use rayon::prelude::*;
    let coefs = &*coefs;
    let scans: Vec<Option<Vec<u8>>> = j
        .scans
        .par_iter()
        .zip(&infos)
        .zip(&raw_data)
        .map(|((s, info), raw)| -> Result<Option<Vec<u8>>, Error> {
            if s.end != s.start {
                return Err(Error("blob contains entropy-coded data".into()));
            }
            if raw.is_some() {
                return Ok(None);
            }
            let mut scan = Vec::new();
            if j.progressive {
                progressive::encode_scan(&j, s, info, coefs, &mut scan)?;
            } else {
                jpeg::encode_scan(&j, s, info, coefs, &mut scan)?;
            }
            Ok(Some(scan))
        })
        .collect::<Result<_, _>>()?;
    let total: usize = scans.iter().flatten().map(|v| v.len()).sum::<usize>()
        + raw_data.iter().flatten().map(|r| r.len()).sum::<usize>();
    let mut out = Vec::with_capacity(blob.len() + total);
    let mut q = 0;
    for ((s, scan), raw) in j.scans.iter().zip(&scans).zip(&raw_data) {
        out.extend_from_slice(&blob[q..s.start]);
        out.extend_from_slice(scan.as_deref().or(*raw).unwrap());
        q = s.start;
    }
    out.extend_from_slice(&blob[q..]);
    *t = Timing { parse: (t1 - t0).as_secs_f64(), model: (t2 - t1).as_secs_f64(), huffman: t2.elapsed().as_secs_f64() };
    Ok(out)
}

fn try_decompress(c: &[u8], t: &mut Timing, depth: u32) -> Result<Vec<u8>, Error> {
    if c.len() < 8 || c[..6] != MAGIC {
        return Err(Error("not a JRC container".into()));
    }
    let version = u16::from_le_bytes([c[6], c[7]]);
    let mut r = Streams { d: c, p: 8 };
    let progressive = match version {
        V_STORED => {
            let s = r.next()?;
            r.finish()?;
            return Ok(s.to_vec());
        }
        V_SEQUENTIAL => false,
        V_PROGRESSIVE => true,
        _ => return Err(Error(format!("unknown version {version}"))),
    };
    // head: checksum and embedded JPEG entries
    let head = r.next()?;
    let hash = u64::from_le_bytes(head.get(..8).ok_or(Error("truncated".into()))?.try_into().unwrap());
    let mut p = 8;
    let n = get_varint(head, &mut p)?;
    let mut entries = Vec::new(); // (gap, len, Some(back reference))
    for i in 0..n {
        let gap = get_varint(head, &mut p)? as usize;
        let len = get_varint(head, &mut p)? as usize;
        let kind = *head.get(p).ok_or(Error("truncated".into()))?;
        p += 1;
        let back = match kind {
            0 if depth < MAX_DEPTH => None,
            1 => {
                let k = get_varint(head, &mut p)?;
                if k >= i {
                    return Err(Error("bad embedded reference".into()));
                }
                Some(k as usize)
            }
            _ => return Err(Error("bad embedded entry".into())),
        };
        entries.push((gap, len, back));
    }
    fully_read(head, p, "head")?;
    // embedded JPEGs (recursively), then the blob is restored around them
    let mut emb: Vec<Vec<u8>> = Vec::with_capacity(entries.len());
    for &(_, len, back) in &entries {
        let bytes = match back {
            None => try_decompress(r.next()?, &mut Timing::default(), depth + 1)?,
            Some(k) => emb[k].clone(),
        };
        if bytes.len() != len {
            return Err(Error("embedded length mismatch".into()));
        }
        emb.push(bytes);
    }
    let cut = r.next()?;
    let blob_len = cut.len() + emb.iter().map(|b| b.len()).sum::<usize>();
    let mut blob = Vec::with_capacity(blob_len);
    let mut q = 0usize;
    for ((gap, _, _), bytes) in entries.iter().zip(&emb) {
        let piece = cut.get(q..q.saturating_add(*gap)).ok_or(Error("bad embedded position".into()))?;
        blob.extend_from_slice(piece);
        blob.extend_from_slice(bytes);
        q += gap;
    }
    blob.extend_from_slice(&cut[q..]);
    let out = with_bufs(|coefs, _| decompress_core(&blob, progressive, &mut r, t, coefs))?;
    if xxhash_rust::xxh3::xxh3_64(&out) != hash {
        return Err(Error("checksum mismatch: reconstruction differs from the original".into()));
    }
    Ok(out)
}

pub fn decompress_with(c: &[u8], t: &mut Timing) -> Result<Vec<u8>, Error> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| try_decompress(c, t, 0))) {
        Ok(r) => r,
        Err(_) => Err(Error("internal panic".into())),
    }
}

pub fn decompress(c: &[u8]) -> Result<Vec<u8>, Error> {
    decompress_with(c, &mut Timing::default())
}
