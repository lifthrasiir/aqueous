//! Container format and the top-level compress / decompress entry points.
//!
//! ```text
//! "JRC" version(u8=4) mode(u8)
//! mode 0 (stored): the input verbatim
//! mode 1 (coded):
//!   u64 LE xxh3 of the original input (checked after decompression: a reconstruction that
//!   differs is reported as an error instead of being returned)
//!   varint n_embedded, per embedded JPEG (ascending positions in the blob):
//!       varint gap (bytes of blob before it, after the previous one), varint length,
//!       u8 kind: 0 = varint len + a nested JRC stream, 1 = varint index of an identical
//!       earlier embedded JPEG
//!   varint cut_len, cut blob   -- the blob with the embedded JPEGs removed
//!   varint nscans, per scan: u8 flags (bit 0: zero padding, bit 1: truncated,
//!       bit 2: truncated at a segment boundary, bit 3: raw),
//!       [if raw: varint length, the scan's entropy-coded data verbatim]
//!       [else: varint trailing RST count, [if truncated: varint MCUs, varint bit
//!       position, varint tail length, tail bytes]]
//!   fast-model payload (tables, then per stripe: rANS words, raw bits)
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
use std::time::Instant;

const MAGIC: &[u8; 3] = b"JRC";
const VERSION: u8 = 4;
const MODE_STORED: u8 = 0;
const MODE_CODED: u8 = 1;
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

fn stored(input: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(input.len() + 5);
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&[VERSION, MODE_STORED]);
    out.extend_from_slice(input);
    out
}

fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push(v as u8 | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
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

/// Main-image part of a coded stream: blob, per-scan section and model payload.
struct Core {
    blob: Vec<u8>,
    scans: Vec<u8>,
    nraw: usize,
}

fn compress_core(input: &[u8], t: &mut Timing, coefs: &mut Vec<Coefs>, clean: &mut Clean) -> Result<Core, Error> {
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
    let (e, _) = fast::encode(&j, coefs);
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
    let mut out = Vec::with_capacity(e.size() + 64);
    put_varint(&mut out, j.scans.len() as u64);
    for (s, i) in j.scans.iter().zip(&infos) {
        let t = i.truncated.as_ref();
        out.push(i.pad_zero as u8 | (t.is_some() as u8) << 1 | (t.is_some_and(|t| t.boundary) as u8) << 2 | (i.raw as u8) << 3);
        if i.raw {
            put_varint(&mut out, (s.end - s.start) as u64);
            out.extend_from_slice(&input[s.start..s.end]);
            continue;
        }
        put_varint(&mut out, i.trailing_rst as u64);
        if let Some(t) = t {
            put_varint(&mut out, t.mcus as u64);
            put_varint(&mut out, t.bits as u64);
            put_varint(&mut out, t.tail.len() as u64);
            out.extend_from_slice(&t.tail);
        }
    }
    e.write(&mut out);
    *t = Timing { parse: (t1 - t0).as_secs_f64(), huffman: (t2 - t1).as_secs_f64(), model: (t3 - t2).as_secs_f64() };
    Ok(Core { blob, scans: out, nraw: infos.iter().filter(|i| i.raw).count() })
}

fn try_compress(input: &[u8], t: &mut Timing, depth: u32) -> Result<(Vec<u8>, usize), Error> {
    let core = with_bufs(|coefs, clean| compress_core(input, t, coefs, clean))?;
    let mut out = Vec::with_capacity(core.blob.len() + core.scans.len() + 64);
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&[VERSION, MODE_CODED]);
    out.extend_from_slice(&xxhash_rust::xxh3::xxh3_64(input).to_le_bytes());
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
            if let Ok((c, _)) = try_compress(bytes, &mut Timing::default(), depth + 1) {
                if c.len() + 8 < bytes.len() {
                    taken.push((a, b));
                    entries.push((a, b, Some(c), 0));
                }
            }
        }
    }
    put_varint(&mut out, entries.len() as u64);
    let mut prev = 0;
    for (a, b, c, k) in &entries {
        put_varint(&mut out, (a - prev) as u64);
        put_varint(&mut out, (b - a) as u64);
        match c {
            Some(c) => {
                out.push(0);
                put_varint(&mut out, c.len() as u64);
                out.extend_from_slice(c);
            }
            None => {
                out.push(1);
                put_varint(&mut out, *k as u64);
            }
        }
        prev = *b;
    }
    let cut_len = core.blob.len() - entries.iter().map(|(a, b, _, _)| b - a).sum::<usize>();
    put_varint(&mut out, cut_len as u64);
    let mut p = 0;
    for (a, b, _, _) in &entries {
        out.extend_from_slice(&core.blob[p..*a]);
        p = *b;
    }
    out.extend_from_slice(&core.blob[p..]);
    out.extend_from_slice(&core.scans);
    Ok((out, core.nraw))
}

/// Compress; never fails (falls back to storing the input). Also returns why it stored.
pub fn compress_with(input: &[u8], t: &mut Timing) -> (Vec<u8>, StoreReason) {
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| try_compress(input, t, 0)));
    match r {
        Ok(Ok((v, nraw))) if v.len() < input.len() + 5 => (v, (nraw > 0).then(|| format!("partial: {nraw} raw scan(s)"))),
        Ok(Ok(_)) => (stored(input), Some("no gain".into())),
        Ok(Err(e)) => (stored(input), Some(e.0)),
        Err(_) => (stored(input), Some("internal panic".into())),
    }
}

pub fn compress(input: &[u8]) -> Vec<u8> {
    compress_with(input, &mut Timing::default()).0
}

// ---------------------------------------------------------------- decompression

fn decompress_core(blob: &[u8], c: &[u8], mut p: usize, t: &mut Timing, coefs: &mut Vec<Coefs>) -> Result<Vec<u8>, Error> {
    let t0 = Instant::now();
    let j = jpeg::parse(blob)?;
    let ns = get_varint(c, &mut p)?;
    if ns != j.scans.len() as u64 {
        return Err(Error("scan count mismatch".into()));
    }
    let mut infos = Vec::with_capacity(j.scans.len());
    let mut raws = Vec::with_capacity(j.scans.len());
    for _ in 0..ns {
        let f = *c.get(p).ok_or(Error("truncated".into()))?;
        p += 1;
        if f & 8 != 0 {
            let n = get_varint(c, &mut p)?;
            raws.push(Some(get_bytes(c, &mut p, n)?));
            infos.push(ScanInfo { raw: true, ..Default::default() });
            continue;
        }
        raws.push(None);
        let trailing_rst = u32::try_from(get_varint(c, &mut p)?).map_err(|_| Error("bad RST count".into()))?;
        let truncated = if f & 2 != 0 {
            let mcus = get_varint(c, &mut p)? as usize;
            let bits = get_varint(c, &mut p)? as usize;
            let n = get_varint(c, &mut p)?;
            let tail = get_bytes(c, &mut p, n)?.to_vec();
            Some(jpeg::Truncation { mcus, bits, tail, boundary: f & 4 != 0 })
        } else {
            None
        };
        infos.push(ScanInfo { standard: true, trailing_rst, pad_zero: f & 1 != 0, truncated, raw: false });
    }
    let e = fast::Encoded::read(c, &mut p)?;
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
        .zip(&raws)
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
    let total: usize = scans.iter().flatten().map(|v| v.len()).sum::<usize>() + raws.iter().flatten().map(|r| r.len()).sum::<usize>();
    let mut out = Vec::with_capacity(blob.len() + total);
    let mut q = 0;
    for ((s, scan), raw) in j.scans.iter().zip(&scans).zip(&raws) {
        out.extend_from_slice(&blob[q..s.start]);
        out.extend_from_slice(scan.as_deref().or(*raw).unwrap());
        q = s.start;
    }
    out.extend_from_slice(&blob[q..]);
    *t = Timing { parse: (t1 - t0).as_secs_f64(), model: (t2 - t1).as_secs_f64(), huffman: t2.elapsed().as_secs_f64() };
    Ok(out)
}

fn try_decompress(c: &[u8], t: &mut Timing, depth: u32) -> Result<Vec<u8>, Error> {
    if c.len() < 5 || &c[..3] != MAGIC || c[3] != VERSION {
        return Err(Error("not a JRC stream".into()));
    }
    match c[4] {
        MODE_STORED => return Ok(c[5..].to_vec()),
        MODE_CODED => {}
        _ => return Err(Error("unknown mode".into())),
    }
    let hash = u64::from_le_bytes(c.get(5..13).ok_or(Error("truncated".into()))?.try_into().unwrap());
    // embedded JPEGs first (recursively), then the blob is restored around them
    let mut p = 13;
    let n = get_varint(c, &mut p)?;
    let mut emb: Vec<(usize, Vec<u8>)> = Vec::new(); // (gap, bytes)
    for _ in 0..n {
        let gap = get_varint(c, &mut p)? as usize;
        let len = get_varint(c, &mut p)? as usize;
        let kind = *c.get(p).ok_or(Error("truncated".into()))?;
        p += 1;
        let bytes = match kind {
            0 if depth < MAX_DEPTH => {
                let m = get_varint(c, &mut p)?;
                try_decompress(get_bytes(c, &mut p, m)?, &mut Timing::default(), depth + 1)?
            }
            1 => {
                let k = get_varint(c, &mut p)? as usize;
                emb.get(k).ok_or(Error("bad embedded reference".into()))?.1.clone()
            }
            _ => return Err(Error("bad embedded entry".into())),
        };
        if bytes.len() != len {
            return Err(Error("embedded length mismatch".into()));
        }
        emb.push((gap, bytes));
    }
    let m = get_varint(c, &mut p)?;
    let cut = get_bytes(c, &mut p, m)?;
    let blob_len = cut.len() + emb.iter().map(|(_, b)| b.len()).sum::<usize>();
    let mut blob = Vec::with_capacity(blob_len);
    let mut q = 0;
    for (gap, bytes) in &emb {
        let piece = cut.get(q..q + gap).ok_or(Error("bad embedded position".into()))?;
        blob.extend_from_slice(piece);
        blob.extend_from_slice(bytes);
        q += gap;
    }
    blob.extend_from_slice(&cut[q..]);
    let out = with_bufs(|coefs, _| decompress_core(&blob, c, p, t, coefs))?;
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
