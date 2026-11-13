//! The codec behind the formula module (specified in FORMAT.md): the main image's split into a
//! blob, scan info and modelled coefficients, the embedded JPEGs of the blob, and the inverse.
//!
//! The blob is the input with every scan's entropy-coded data removed (headers, inter-scan
//! markers, trailer); it parses as a JPEG whose scans are empty. JPEGs embedded in it (Exif and
//! Photoshop thumbnails, MPF / vendor secondary images in the trailer, ...) are coded
//! separately. Decoding restores the blob, re-emits each scan from the decoded coefficients
//! and splices it back in.
//!
//! Sequential files with a single scan (most files) are decoded, modelled and re-emitted a
//! stripe at a time, so that memory does not follow the image size: only a few stripes'
//! coefficients are held at once (see `jpeg::decode_striped`, `model::Modeller` and
//! `jpeg::encode_striped`). Other files, and scans that do not decode cleanly, are held whole.

use crate::jpeg::{self, Clean, Coefs, Error, Jpeg, Scan, ScanInfo};
use crate::model::{self, get_varint};
use crate::progressive;
#[cfg(feature = "encode")]
use aqr_common::varint;
#[cfg(feature = "encode")]
use std::borrow::Cow;

/// What decoding re-emits that the streams do not tell, measured by the encoder (jpeg-recons
/// 3's params, after the frame type): of a scan coded a stripe at a time, the bits of each coded
/// stripe's mid and body (`jpeg::encode_striped`); else the unstuffed bytes of each scan that is
/// not raw, in order. They are the room of the bit writers decoding takes, which it holds to.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Sizes {
    pub stripes: Vec<(usize, usize)>,
    pub scans: Vec<usize>,
}

/// scan flags (progressive scans use only the first two)
const F_PAD_ZERO: u8 = 1;
pub(crate) const F_RAW: u8 = 2;
pub(crate) const F_TRUNCATED: u8 = 4;
const F_BOUNDARY: u8 = 8;
/// nesting limit for embedded JPEGs (main image = depth 0)
#[cfg(feature = "encode")]
const MAX_DEPTH: u32 = 3;
/// embedded JPEGs smaller than this are left alone (model tables alone would outweigh them)
#[cfg(feature = "encode")]
const MIN_EMBEDDED: usize = 1024;

#[cfg(feature = "encode")]
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
    let bytes = coefs.iter().map(|c| c.blocks.capacity() * 128 + c.masks.capacity() * 8).sum::<usize>();
    if bytes > jpeg::KEEP_BYTES {
        coefs = Vec::new();
    }
    if clean.data.capacity() > jpeg::KEEP_BYTES {
        clean = Clean::default();
    }
    BUFS.with(|b| *b.borrow_mut() = (coefs, clean));
    r
}

// ---------------------------------------------------------------- embedded JPEGs

/// End (exclusive) of a JPEG starting with SOI at `p`, following its marker segments and
/// entropy-coded data up to EOI.
#[cfg(feature = "encode")]
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
#[cfg(feature = "encode")]
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
#[cfg(feature = "encode")]
pub(crate) struct Core<'a> {
    pub progressive: bool,
    pub sizes: Sizes,
    pub blob: Vec<u8>,
    pub info: Vec<u8>,
    /// raw scan data / truncated tails, in scan order
    pub scans: Vec<Cow<'a, [u8]>>,
    pub model: model::Encoded,
}

/// The scan of a sequential file that codes every component in one scan (as most do), whose
/// MCU grid is the components' block grid: such files are coded a stripe at a time.
fn striped_scan(j: &Jpeg) -> Option<&Scan> {
    let [s] = &j.scans[..] else { return None };
    if j.progressive || s.comps.len() != j.comps.len() || (s.ss, s.se, s.ah, s.al) != (0, 63, 0, 0) {
        return None;
    }
    let single = j.comps.len() == 1;
    j.comps
        .iter()
        .all(|c| {
            let (h, v) = if single { (1, 1) } else { (c.h, c.v) };
            c.bw == s.mcux * h && c.bh == s.mcuy * v
        })
        .then_some(s)
}

/// Scan info and (for a reproducible scan) model of a `striped_scan` file, its coefficients
/// decoded and modelled a stripe at a time; None for other files.
#[cfg(feature = "encode")]
#[allow(clippy::type_complexity)]
fn model_striped(
    j: &Jpeg,
    input: &[u8],
    clean: &mut Clean,
) -> Option<(ScanInfo, Option<(model::Encoded, Vec<(usize, usize)>)>)> {
    let s = striped_scan(j)?;
    jpeg::destuff(&input[s.start..s.end], clean);
    let ns = model::stripe_count(j);
    let ranges = model::stripe_mcus(j, s.mcux, ns);
    let pool = std::sync::Mutex::new(Vec::<Vec<Coefs>>::new());
    for serial_only in [false, true] {
        let m = model::Modeller::new(j, ns);
        // each coded stripe's bits as decoding codes it again (None where it fails)
        let bits = std::sync::Mutex::new(vec![None; ns]);
        let take = |k| {
            let mut b = pool.lock().unwrap().pop().unwrap_or_default();
            model::hold_stripe(j, k, ns, &mut b);
            b
        };
        let put = |k: usize, b: Vec<Coefs>, trunc: Option<(usize, bool)>| {
            m.stripe(j, k, &b);
            if let Ok(st) = jpeg::Striped::new(j, s, trunc, false)
                && let Some(r) = st.coded(&ranges).nth(k)
            {
                bits.lock().unwrap()[k] = st.part_bits(r, &b).ok();
            }
            pool.lock().unwrap().push(b);
        };
        match jpeg::decode_striped(j, s, clean, &ranges, serial_only, take, put) {
            Some(info) if !info.standard => return Some((info, None)),
            Some(info) => {
                let st = jpeg::Striped::new(j, s, info.truncated.as_ref().map(|t| (t.mcus, t.boundary)), false).ok()?;
                let n = st.coded(&ranges).count();
                let bits: Option<Vec<_>> = bits.into_inner().unwrap().into_iter().take(n).collect();
                return Some((info, Some((m.finish().0, bits?))));
            }
            None => {} // start over serially
        }
    }
    unreachable!("serial stripe-wise decoding always gives an outcome")
}

#[cfg(feature = "encode")]
pub(crate) fn encode_core<'a>(input: &'a [u8], coefs: &mut Vec<Coefs>, clean: &mut Clean) -> Result<Core<'a>, Error> {
    let j = jpeg::parse(input)?;
    let none = || Err(Error("no scan could be modelled".into()));
    let (infos, model, sizes) = match model_striped(&j, input, clean) {
        Some((_, None)) => {
            if std::env::var_os("JRC_VERIFY_STRIPED").is_some()
                && let Ok(infos) = jpeg::decode_all(&j, input, coefs, clean)
            {
                // test oracle: decoding the scan whole must not take it either
                assert!(infos.iter().all(|i| i.raw), "striped decoding: rejected a reproducible scan");
            }
            return none();
        }
        Some((info, Some((model, bits)))) => {
            if std::env::var_os("JRC_VERIFY_STRIPED").is_some() {
                // test oracle: the outcome must equal that of decoding the scan whole
                let infos = jpeg::decode_all(&j, input, coefs, clean)?;
                assert!(infos == [info.clone()], "striped decoding: scan info differs");
                let e = model::encode(&j, coefs).0;
                let same = e.tables == model.tables
                    && e.stripes.len() == model.stripes.len()
                    && e.stripes.iter().zip(&model.stripes).all(|(a, b)| a.rans == b.rans && a.raw == b.raw);
                assert!(same, "striped decoding: model differs");
            }
            (vec![info], model, Sizes { stripes: bits, scans: Vec::new() })
        }
        None => {
            check_whole(&j, input.len())?;
            let infos = if j.progressive {
                progressive::decode_all(&j, input, coefs, clean)?
            } else {
                jpeg::decode_all(&j, input, coefs, clean)?
            };
            if infos.iter().all(|i| i.raw) {
                return none();
            }
            // each scan's unstuffed bytes, as decoding codes it again: those of the scan itself
            // (destuffed, without its RST markers) where it is reproduced as it is, those coded
            // again of one cut short
            let mut scans = Vec::new();
            for (s, i) in j.scans.iter().zip(&infos) {
                match (i.raw, &i.truncated) {
                    (true, _) => {}
                    (false, None) => {
                        jpeg::destuff(&input[s.start..s.end], clean);
                        scans.push(clean.data.len() - jpeg::PAD);
                    }
                    (false, Some(_)) => scans.push(scan_bytes(&j, s, i, coefs)?),
                }
            }
            (infos, model::encode(&j, coefs).0, Sizes { stripes: Vec::new(), scans })
        }
    };
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
    Ok(Core { progressive: j.progressive, sizes, blob, info, scans, model })
}

/// An embedded JPEG of the blob: the byte range, and either its coding `coded` or (None) the
/// index `back` of an earlier entry with the same bytes.
#[cfg(feature = "encode")]
pub(crate) struct Entry<C> {
    pub start: usize,
    pub end: usize,
    pub coded: Option<C>,
    pub back: usize,
}

/// The embedded JPEGs of `blob` to be coded at `depth`: those `code` gives a coding for (only
/// where it pays off), and repeats of earlier entries.
#[cfg(feature = "encode")]
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

/// `head` gets the entry list: count, then per entry gap and kind (+ back reference).
#[cfg(feature = "encode")]
pub(crate) fn put_entries<C>(head: &mut Vec<u8>, entries: &[Entry<C>]) {
    put_varint(head, entries.len() as u64);
    let mut prev = 0;
    for e in entries {
        put_varint(head, (e.start - prev) as u64);
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
#[cfg(feature = "encode")]
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

/// Blocks the components held whole may have, for each byte of the JPEG: a block takes a bit of
/// its DC at least, so a header of more blocks than that over little data is a lie (and the
/// file is kept as it is, or the archive corrupt), not 2 GiB of coefficients to reserve.
const BLOCKS_PER_BYTE: usize = 8;
/// Blocks allowed whatever the length (a truncated file may have few bytes of many blocks).
const BLOCKS_FREE: usize = 1 << 16;

/// Whether components of `j` may be held whole for a JPEG of `len` bytes.
fn check_whole(j: &Jpeg, len: usize) -> Result<(), Error> {
    let blocks: usize = j.comps.iter().map(|c| c.bw * c.bh).sum();
    if blocks > len.saturating_mul(BLOCKS_PER_BYTE).saturating_add(BLOCKS_FREE) {
        return Err(Error("more blocks than the data holds".into()));
    }
    Ok(())
}

/// Whether the sizes and the restart markers after the scans, as the params and the scan info
/// tell them, fit in the output of `len` bytes (which holds each of them as it is, or more).
fn check_sizes(sizes: &Sizes, trailing: impl Iterator<Item = usize>, len: usize) -> Result<(), Error> {
    let bad = sizes.scans.iter().any(|&n| n > len)
        || sizes.stripes.iter().any(|&(a, b)| a / 8 > len || b / 8 > len)
        || trailing.into_iter().any(|t| t > len / 2);
    if bad { Err(Error("sizes beyond the output".into())) } else { Ok(()) }
}

/// `out` with room for `len` bytes more, if it can be had.
pub(crate) fn reserve_output(out: &mut Vec<u8>, len: usize) -> Result<(), Error> {
    out.try_reserve_exact(len).map_err(|_| Error("output too large".into()))
}

/// The entry list of a head, as (gap, back reference) with None for coded entries.
pub(crate) fn parse_entries(head: &[u8], p: &mut usize) -> Result<Vec<(usize, Option<usize>)>, Error> {
    let n = get_varint(head, p)?;
    // (an entry takes two bytes at least)
    if n > head.len() as u64 {
        return Err(Error("bad entry count".into()));
    }
    let mut entries = Vec::with_capacity(n as usize);
    for i in 0..n {
        let gap = get_varint(head, p)? as usize;
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
        entries.push((gap, back));
    }
    Ok(entries)
}

/// The blob: `cut` with the bytes of each entry put back at its gap.
pub(crate) fn restore_blob<E: AsRef<[u8]>>(
    cut: &[u8],
    entries: &[(usize, Option<usize>)],
    emb: &[E],
) -> Result<Vec<u8>, Error> {
    let mut blob = Vec::with_capacity(cut.len() + emb.iter().map(|b| b.as_ref().len()).sum::<usize>());
    let mut q = 0usize;
    for ((gap, _), bytes) in entries.iter().zip(emb) {
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
    (sizes, len): (&Sizes, usize),
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
    let mut tails = Vec::with_capacity(j.scans.len()); // (scan, mcus, bits, boundary)
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
    check_sizes(sizes, infos.iter().map(|i| i.trailing_rst as usize), len)?;
    let nstripes = get_varint(info, &mut p)?;
    fully_read(info, p, "scan info")?;
    if nstripes == 0 || nstripes > model::MAX_STRIPES as u64 {
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
    let mut stripes: Vec<(&[u8], &[u8])> = Vec::with_capacity(nstripes as usize);
    for _ in 0..nstripes {
        stripes.push(r.stripe()?);
    }
    r.finish()?;
    let t = model::read_tables_section(tables)?;
    let mut out = Vec::new();
    if let Some(s) = striped_scan(&j)
        && !raws[0]
        && infos[0].truncated.as_ref().is_none_or(|t| t.mcus > 0)
    {
        // a stripe at a time
        if s.end != s.start || !sizes.scans.is_empty() {
            return Err(Error("blob contains entropy-coded data".into()));
        }
        let ns = stripes.len();
        model::check_stripes(&j, ns)?;
        let ranges = model::stripe_mcus(&j, s.mcux, ns);
        let fill = |k, b: &mut Vec<Coefs>| model::decode_stripe(&j, &t, k, ns, stripes[k], b);
        jpeg::encode_striped(&j, s, &infos[0], &ranges, &sizes.stripes, fill, (blob, len), &mut out)?;
        if std::env::var_os("JRC_VERIFY_STRIPED").is_some() {
            // test oracle: the scan must equal the one re-emitted from whole components
            model::decode_whole(&j, &t, &stripes, coefs)?;
            let mut scan = Vec::new();
            jpeg::encode_scan(&j, s, &infos[0], coefs, &mut scan)?;
            assert!(out[s.start..out.len() - (blob.len() - s.start)] == scan, "striped re-emission differs");
        }
        // (in WebAssembly what decoding made is left as it is: below the output, each a hole)
        #[cfg(target_family = "wasm")]
        std::mem::forget((ranges, t, stripes, raw_data, raws, infos, j));
        return Ok(out);
    }
    check_whole(&j, len)?;
    model::decode_whole(&j, &t, &stripes, coefs)?;
    // the scans re-emitted in turn onto the output (reserved after the coefficients), through
    // one bit writer of room for the largest
    let coded = j.scans.iter().zip(&raw_data).filter(|(_, r)| r.is_none());
    if coded.clone().count() != sizes.scans.len() {
        return Err(Error("scan sizes of another count".into()));
    }
    let room = coded.zip(&sizes.scans).map(|((s, _), &n)| writer_len(&j, s, n)).max().unwrap_or(0);
    reserve_output(&mut out, len)?;
    let mut buf = vec![0u8; room];
    let mut be = if j.progressive { progressive::corrections() } else { Vec::new() };
    let (mut q, mut sz) = (0, sizes.scans.iter());
    for ((s, info), raw) in j.scans.iter().zip(&infos).zip(&raw_data) {
        if s.end != s.start {
            return Err(Error("blob contains entropy-coded data".into()));
        }
        out.extend_from_slice(blob.get(q..s.start).ok_or(Error("bad scan position".into()))?);
        q = s.start;
        if let Some(raw) = raw {
            out.extend_from_slice(raw);
            continue;
        }
        let n = *sz.next().expect("counted");
        if buf.len() < writer_len(&j, s, n) {
            return Err(Error("bad scan size".into()));
        }
        let pos = if j.progressive {
            progressive::encode_onto(&j, s, info, coefs, &mut buf, &mut be, &mut out)?
        } else {
            jpeg::encode_scan_onto(&j, s, info, coefs, &mut buf, &mut out)?
        };
        if pos != n || out.len() > len {
            return Err(Error("bad scan size".into()));
        }
    }
    out.extend_from_slice(&blob[q..]);
    #[cfg(target_family = "wasm")]
    std::mem::forget((j, infos, raws, raw_data, stripes, t, buf, be));
    Ok(out)
}

/// The memory plan of [`decode_core`] on WebAssembly ([`aqr_api::stack`]), of the same blob,
/// frame type, streams (`r`), sizes and output (`len` bytes), given `top` bytes of the stack
/// taken before (the instance's initial memory with it): the most the instance takes and the
/// most a task does, in bytes. As `decode_core`
/// takes it: the parsed JPEG, the scans' info, the stripes' streams listed, the tables; then
/// a scan coded a stripe at a time (each stripe a task, the instance taking each part, then the
/// output, a part put together at a time), or the components whole, the output, the bit writer
/// and a scan's tables at a time.
pub(crate) fn core_plan<'a>(
    blob: &[u8],
    progressive: bool,
    r: &mut impl CoreSource<'a>,
    (sizes, len): (&Sizes, usize),
    mut top: u64,
) -> Option<(u64, u64)> {
    use aqr_api::stack::block;
    let pages = |n: u64| n.div_ceil(1 << 16) << 16;
    let j = jpeg::parse(blob).ok()?;
    if j.progressive != progressive {
        return None;
    }
    let n = |k: usize| k as u64;
    let ns = j.scans.len();
    top += block(36 * n(ns)) + block(32 * n(j.comps.len()));
    top += j.scans.iter().map(|s| block(556 * n(s.comps.len()))).sum::<u64>();
    top += block(32 * n(ns)) + block(n(ns)) + block(16 * n(ns)) + block(8 * n(ns));
    // the scans' info, as `decode_core` reads it (the tails' bytes, and each scan raw or not)
    let info = r.info().ok()?;
    let mut p = 0;
    get_varint(info, &mut p).ok()?;
    let (mut flags, mut trailing) = (Vec::with_capacity(ns), Vec::with_capacity(ns));
    for _ in 0..ns {
        let f = *info.get(p)?;
        p += 1;
        trailing.push(0);
        if f & F_RAW == 0 {
            *trailing.last_mut()? = usize::try_from(get_varint(info, &mut p).ok()?).ok()?;
            if f & F_TRUNCATED != 0 {
                get_varint(info, &mut p).ok()?;
                get_varint(info, &mut p).ok()?;
            }
        }
        flags.push(f);
    }
    let nstripes = usize::try_from(get_varint(info, &mut p).ok()?).ok()?;
    check_sizes(sizes, trailing.iter().copied(), len).ok()?;
    let (mut trunc_tail, mut raws) = (0, Vec::with_capacity(ns));
    for &f in &flags {
        raws.push(f & F_RAW != 0);
        if f & F_RAW != 0 {
            r.scan().ok()?;
        } else if f & F_TRUNCATED != 0 {
            let t = r.scan().ok()?.len();
            top += block(n(t));
            trunc_tail = t;
        }
    }
    r.tables().ok()?;
    let mut stripes = Vec::with_capacity(nstripes);
    for _ in 0..nstripes {
        let (w, b) = r.stripe().ok()?;
        stripes.push((w.len(), b.len()));
    }
    top += block(16 * n(nstripes)) + model::tables_plan();
    if let Some(s) = striped_scan(&j)
        && !raws[0]
    {
        let trunc = match flags[0] & F_TRUNCATED != 0 {
            true => {
                // (the truncation's MCUs and boundary, read again)
                let mut p = 1;
                get_varint(info, &mut p).ok()?;
                get_varint(info, &mut p).ok()?;
                let mcus = get_varint(info, &mut p).ok()? as usize;
                Some((mcus, flags[0] & F_BOUNDARY != 0))
            }
            false => None,
        };
        if trunc.is_some_and(|t| t.0 == 0) {
            return None;
        }
        let ranges = model::stripe_mcus(&j, s.mcux, nstripes);
        let st = jpeg::Striped::new(&j, s, trunc, false).ok()?;
        let coded: Vec<(usize, usize)> = st.coded(&ranges).collect();
        if coded.len() != sizes.stripes.len() {
            return None;
        }
        let nc = n(s.comps.len());
        top += block(8 * n(nstripes))
            + 2 * block(1024 * nc)
            + block(12 * nc)
            + block(8 * n(coded.len()))
            + block(12 * n(coded.len()));
        let end = pages(top);
        let tasks: Vec<u64> = coded
            .iter()
            .zip(&sizes.stripes)
            .enumerate()
            .map(|(k, (&range, &bits))| {
                let (rows, decode) = model::stripe_plan(&j, k, nstripes, stripes[k]);
                end.max(pages(top + rows + decode.max(st.part_plan(range, bits))))
            })
            .collect();
        top += block(n(coded.len()));
        top +=
            coded.iter().zip(&sizes.stripes).map(|(&range, &bits)| block(n(st.part_written(range, bits)))).sum::<u64>();
        let taken = end.max(pages(top));
        let most = tasks.into_iter().max().unwrap_or(0);
        top += block(n(len));
        let join = coded
            .iter()
            .zip(&sizes.stripes)
            .map(|(&range, &bits)| {
                block(n(st.head_len()))
                    + block(n(st.join_len(bits, trunc_tail)))
                    + block(4 * n(st.body_ends(range) + 1))
            })
            .max()
            .unwrap_or(0);
        return Some((taken.max(pages(top + join)), most));
    }
    check_whole(&j, len).ok()?;
    let (held, decoding) = model::whole_plan(&j, &stripes);
    top += held;
    let decoded = pages(top + decoding);
    let coded = j.scans.iter().zip(&raws).zip(&trailing).filter(|((_, r), _)| !**r).map(|((s, _), &t)| (s, t));
    let room = coded.clone().zip(&sizes.scans).map(|((s, _), &k)| writer_len(&j, s, k)).max().unwrap_or(0);
    top += block(n(len)) + block(n(room)) + if progressive { block(n(progressive::CORRECTIONS)) } else { 0 };
    let scan = coded
        .map(|(s, trailing)| {
            let nc = n(s.comps.len());
            let nmcu = s.mcux * s.mcuy;
            let ri = if s.ri > 0 { s.ri } else { nmcu.max(1) };
            // (an RST offset for each restart interval, and those trailing)
            let ends = block(4 * n(nmcu.div_ceil(ri) + trailing));
            match progressive {
                true => block(4 * 1024) + block(12 * nc) + ends,
                false => 2 * block(1024 * nc) + block(12 * nc) + ends,
            }
        })
        .max()
        .unwrap_or(0);
    Some((decoded.max(pages(top + scan)), 0))
}

/// Bytes of the bit writer re-emitting scan `s` of `n` unstuffed bytes takes (what its writes
/// keep past its position: a block's, an MCU row's of the progressive coder, its last flush).
pub(crate) fn writer_len(j: &Jpeg, s: &Scan, n: usize) -> usize {
    match j.progressive {
        true => n + progressive::row_room(j, s) + 1024 + jpeg::SLACK,
        false => n + jpeg::SLACK,
    }
}

/// The unstuffed bytes of scan `s` as decoding codes it again (of info `info`, from `coefs`).
#[cfg(feature = "encode")]
fn scan_bytes(j: &Jpeg, s: &Scan, info: &ScanInfo, coefs: &[Coefs]) -> Result<usize, Error> {
    let mut out = Vec::new();
    let mut buf = Vec::new();
    match j.progressive {
        true => progressive::encode_onto(j, s, info, coefs, &mut buf, &mut progressive::corrections(), &mut out),
        false => jpeg::encode_scan_onto(j, s, info, coefs, &mut buf, &mut out),
    }
}
