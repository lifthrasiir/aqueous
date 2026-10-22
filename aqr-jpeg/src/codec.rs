//! The codec behind the formula module (specified in FORMAT.md): the main image's split into a
//! blob, scan info and modelled coefficients, the embedded JPEGs of the blob, and the inverse.
//!
//! The blob is the input with every scan's entropy-coded data removed (headers, inter-scan
//! markers, trailer); it parses as a JPEG whose scans are empty. JPEGs embedded in it (Exif and
//! Photoshop thumbnails, MPF / vendor secondary images in the trailer, ...) are coded
//! separately. Decoding restores the blob, re-emits each scan from the decoded coefficients
//! and splices it back in.

use crate::fast::{self, get_varint};
use crate::jpeg::{self, Clean, Coefs, Error, Order, ScanInfo};
use crate::progressive;
use aqr_common::varint;
use std::borrow::Cow;

/// scan flags (progressive scans use only the first two)
const F_PAD_ZERO: u8 = 1;
const F_RAW: u8 = 2;
const F_TRUNCATED: u8 = 4;
const F_BOUNDARY: u8 = 8;
/// nesting limit for embedded JPEGs (main image = depth 0)
const MAX_DEPTH: u32 = 3;
/// embedded JPEGs smaller than this are left alone (model tables alone would outweigh them)
const MIN_EMBEDDED: usize = 1024;

fn put_varint(out: &mut Vec<u8>, v: u64) {
    varint::put(out, v)
}

/// A stream's fields must use it up exactly.
pub(crate) fn fully_read(d: &[u8], p: usize, what: &str) -> Result<(), Error> {
    if p != d.len() { Err(Error(format!("excess data in {what} stream"))) } else { Ok(()) }
}

thread_local! {
    /// coefficient and destuffing buffers, reused across calls
    static BUFS: std::cell::RefCell<(Vec<Coefs>, Clean)> = std::cell::RefCell::new((Vec::new(), Clean::default()));
}

pub(crate) fn with_bufs<R>(f: impl FnOnce(&mut Vec<Coefs>, &mut Clean) -> R) -> R {
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

// ---------------------------------------------------------------- encoding

/// Main-image part of a formula: blob, scan info stream, per-scan streams, model.
pub(crate) struct Core<'a> {
    pub progressive: bool,
    pub blob: Vec<u8>,
    pub info: Vec<u8>,
    /// raw scan data / truncated tails, in scan order
    pub scans: Vec<Cow<'a, [u8]>>,
    pub model: fast::Encoded,
}

pub(crate) fn encode_core<'a>(input: &'a [u8], coefs: &mut Vec<Coefs>, clean: &mut Clean) -> Result<Core<'a>, Error> {
    let j = jpeg::parse(input)?;
    let infos = if j.progressive {
        progressive::decode_all(&j, input, coefs, clean)?
    } else {
        jpeg::decode_all(&j, input, Order::Zigzag, coefs, clean)?
    };
    if infos.iter().all(|i| i.raw) {
        return Err(Error("no scan could be modelled".into()));
    }
    let (model, _) = fast::encode(&j, coefs);
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
    put_varint(&mut info, j.scans.len() as u64);
    for (s, i) in j.scans.iter().zip(infos) {
        if i.raw {
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
    Ok(Core { progressive: j.progressive, blob, info, scans, model })
}

/// An embedded JPEG of the blob: the byte range, and either its coding `coded` or (None) the
/// index `back` of an earlier entry with the same bytes.
pub(crate) struct Entry<C> {
    pub start: usize,
    pub end: usize,
    pub coded: Option<C>,
    pub back: usize,
}

/// The embedded JPEGs of `blob` to be coded at `depth`: those `code` gives a coding for (only
/// where it pays off), and repeats of earlier entries.
pub(crate) fn embedded_entries<C>(blob: &[u8], depth: u32, mut code: impl FnMut(&[u8]) -> Option<C>) -> Vec<Entry<C>> {
    let mut entries: Vec<Entry<C>> = Vec::new();
    if depth < MAX_DEPTH {
        for (a, b) in find_embedded(blob) {
            let bytes = &blob[a..b];
            if let Some(k) = entries.iter().position(|e| blob[e.start..e.end] == *bytes) {
                entries.push(Entry { start: a, end: b, coded: None, back: k });
            } else if let Some(c) = code(bytes) {
                entries.push(Entry { start: a, end: b, coded: Some(c), back: 0 });
            }
        }
    }
    entries
}

/// `head` gets the entry list: count, then per entry gap, length and kind (+ back reference).
pub(crate) fn put_entries<C>(head: &mut Vec<u8>, entries: &[Entry<C>]) {
    put_varint(head, entries.len() as u64);
    let mut prev = 0;
    for e in entries {
        put_varint(head, (e.start - prev) as u64);
        put_varint(head, (e.end - e.start) as u64);
        match e.coded {
            Some(_) => head.push(0),
            None => {
                head.push(1);
                put_varint(head, e.back as u64);
            }
        }
        prev = e.end;
    }
}

/// The blob without the entries.
pub(crate) fn cut_blob<C>(blob: &[u8], entries: &[Entry<C>]) -> Vec<u8> {
    let mut cut = Vec::with_capacity(blob.len() - entries.iter().map(|e| e.end - e.start).sum::<usize>());
    let mut p = 0;
    for e in entries {
        cut.extend_from_slice(&blob[p..e.start]);
        p = e.end;
    }
    cut.extend_from_slice(&blob[p..]);
    cut
}

/// `info` and the scan, table and stripe streams of the main image, in decoding order.
pub(crate) trait CoreSource<'a> {
    fn info(&mut self) -> Result<&'a [u8], Error>;
    /// A raw scan's data or a truncated scan's tail.
    fn scan(&mut self) -> Result<&'a [u8], Error>;
    fn tables(&mut self) -> Result<&'a [u8], Error>;
    /// The rANS words and the raw bits of a stripe.
    fn stripe(&mut self) -> Result<(&'a [u8], &'a [u8]), Error>;
    /// Nothing may be left after the last stripe.
    fn finish(&mut self) -> Result<(), Error>;
}
// ---------------------------------------------------------------- decoding

/// The entry list of a head, as (gap, length, back reference) with None for coded entries.
pub(crate) fn parse_entries(head: &[u8], p: &mut usize) -> Result<Vec<(usize, usize, Option<usize>)>, Error> {
    let n = get_varint(head, p)?;
    let mut entries = Vec::new();
    for i in 0..n {
        let gap = get_varint(head, p)? as usize;
        let len = get_varint(head, p)? as usize;
        let kind = *head.get(*p).ok_or(Error("truncated".into()))?;
        *p += 1;
        let back = match kind {
            0 => None,
            1 => {
                let k = get_varint(head, p)?;
                if k >= i {
                    return Err(Error("bad embedded reference".into()));
                }
                Some(k as usize)
            }
            _ => return Err(Error("bad embedded entry".into())),
        };
        entries.push((gap, len, back));
    }
    Ok(entries)
}

/// The blob: `cut` with the bytes of each entry put back at its gap.
pub(crate) fn restore_blob<E: AsRef<[u8]>>(
    cut: &[u8],
    entries: &[(usize, usize, Option<usize>)],
    emb: &[E],
) -> Result<Vec<u8>, Error> {
    let mut blob = Vec::with_capacity(cut.len() + emb.iter().map(|b| b.as_ref().len()).sum::<usize>());
    let mut q = 0usize;
    for ((gap, _, _), bytes) in entries.iter().zip(emb) {
        let piece = cut.get(q..q.saturating_add(*gap)).ok_or(Error("bad embedded position".into()))?;
        blob.extend_from_slice(piece);
        blob.extend_from_slice(bytes.as_ref());
        q += gap;
    }
    blob.extend_from_slice(&cut[q..]);
    Ok(blob)
}

pub(crate) fn decode_core<'a>(
    blob: &[u8],
    progressive: bool,
    r: &mut impl CoreSource<'a>,
    coefs: &mut Vec<Coefs>,
) -> Result<Vec<u8>, Error> {
    let j = jpeg::parse(blob)?;
    if j.progressive != progressive {
        return Err(Error("frame type does not match".into()));
    }
    let info = r.info()?;
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
            raw_data[k] = Some(r.scan()?);
        } else if let Some(&(_, mcus, bits, boundary)) = tails.peek().filter(|x| x.0 == k) {
            tails.next();
            let tail = r.scan()?.to_vec();
            infos[k].truncated = Some(jpeg::Truncation { mcus, bits, tail, boundary });
        }
    }
    let tables = r.tables()?;
    let stripes: Vec<(&[u8], &[u8])> = (0..nstripes).map(|_| r.stripe()).collect::<Result<_, Error>>()?;
    r.finish()?;
    let e = fast::Encoded::from_parts(tables, &stripes)?;
    fast::decode_into(&j, &e, coefs)?;
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
    Ok(out)
}
