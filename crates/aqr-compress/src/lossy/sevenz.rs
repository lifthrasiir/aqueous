//! 7z archives: `coding`, the stream of each folder whose packed stream an LZMA or LZMA2 coder
//! takes (filters such as BCJ or Delta may follow it in the chain: the stream's data is the
//! filtered data, coded as it is) made again by [`aqr_lzma::canonical`]. The archive is then laid
//! out again as it was: the packed streams one after another (and an encoded header's after
//! them), the header with the sizes of its packed streams (and their CRC-32s, where it has them)
//! written again and the rest of its bytes as they were, the signature header. An encoded header
//! is coded again the same way (its LZMA or LZMA2 coder, its properties kept). Folders of other
//! coders (BCJ2, PPMd, BZip2, Deflate, AES) are kept as they are; archives with encrypted or
//! unknown header coders, data before or after the header, packed streams not following one
//! another, or what aqr-7z does not parse are kept as they are.

use std::ops::Range;

use aqr_7z::header::{self, Coder, Folder, SIGNATURE, Top, id};
use aqr_common::crc32;
use aqr_lzma::lzma::{self, Props};
use aqr_lzma::{canonical, lzma2};

use crate::Options;

/// The most bytes of data of a folder made again (the same as `lossy:max-data`'s default).
const MAX_DATA: usize = 1 << 30;
/// LZMA2's properties where its first LZMA chunk gives none (all of its chunks stored).
const DEFAULT_PROPS: Props = Props { lc: 3, lp: 0, pb: 2 };

/// The data of an LZMA or LZMA2 stream, and what makes it again.
struct Layer {
    data: Vec<u8>,
    props: Props,
    dict: u32,
}

fn le32(d: &[u8]) -> u32 {
    u32::from_le_bytes(d.try_into().unwrap())
}

/// Whether `c` is a coder this makes again.
fn supported(c: &Coder) -> bool {
    c.ins == 1 && c.outs == 1 && (c.id == header::LZMA || c.id == header::LZMA2)
}

/// Decodes `packed`, the whole stream of `c` (LZMA or LZMA2) of `size` bytes of data.
fn decode(c: &Coder, packed: &[u8], size: u64) -> Result<Layer, String> {
    let size = usize::try_from(size).ok().filter(|&n| n < MAX_DATA).ok_or("over 1 GiB of data in a folder")?;
    let bad = |_| "a stream not decoded";
    let layer = if c.id == header::LZMA {
        if c.props.len() != 5 {
            return Err("LZMA properties not of 5 bytes".into());
        }
        let props = Props::from_byte(c.props[0]).ok_or("LZMA properties not right")?;
        // (liblzma, which the check is by, takes no more)
        if props.lc + props.lp > 4 {
            return Err("LZMA's lc + lp over 4".into());
        }
        let dec = lzma::decode(packed, props, Some(size as u64), MAX_DATA).map_err(bad)?;
        if dec.used != packed.len() {
            return Err("data after a stream".into());
        }
        Layer { data: dec.data, props, dict: le32(&c.props[1..5]) }
    } else {
        let &[b] = &c.props[..] else { return Err("LZMA2 properties not of 1 byte".into()) };
        let dict = lzma2::dict_size(b).ok_or("LZMA2 properties not right")?;
        let dec = lzma2::decode(packed, MAX_DATA).map_err(bad)?;
        if dec.used != packed.len() {
            return Err("data after a stream".into());
        }
        let props = dec.segments.iter().flat_map(|s| &s.chunks).find_map(|c| c.props).unwrap_or(DEFAULT_PROPS);
        let mut data = Vec::with_capacity(size);
        for s in &dec.segments {
            data.extend_from_slice(&s.data);
        }
        Layer { data, props, dict }
    };
    if layer.data.len() != size {
        return Err("a stream's data not of the size the header tells".into());
    }
    Ok(layer)
}

/// The stream of `c` made again of `l`.
fn remake(c: &Coder, l: &Layer) -> Vec<u8> {
    if c.id == header::LZMA {
        canonical::lzma(&l.data, l.props, l.dict, false)
    } else {
        canonical::lzma2(&l.data, l.props, l.dict)
    }
}

/// The NUMBER `v`, as it is in the header (the shortest).
fn put_number(out: &mut Vec<u8>, v: u64) {
    let n = (0..8).find(|&n| v < 1u64 << (7 * (n + 1))).unwrap_or(8);
    let first = if n == 8 { 0xff } else { (0xff00u16 >> n) as u8 | (v >> (8 * n)) as u8 };
    out.push(first);
    out.extend(&v.to_le_bytes()[..n]);
}

/// A NUMBER of `h` at `*p`.
fn number(h: &[u8], p: &mut usize) -> Result<u64, String> {
    let cut = || "a header cut".to_string();
    let first = *h.get(*p).ok_or_else(cut)?;
    *p += 1;
    let mut v = 0u64;
    for i in 0..8 {
        let mask = 0x80u8 >> i;
        if first & mask == 0 {
            return Ok(v | ((first & mask.wrapping_sub(1)) as u64) << (8 * i));
        }
        v |= (*h.get(*p).ok_or_else(cut)? as u64) << (8 * i);
        *p += 1;
    }
    Ok(v)
}

fn byte(h: &[u8], p: &mut usize) -> Result<u8, String> {
    let b = *h.get(*p).ok_or("a header cut")?;
    *p += 1;
    Ok(b)
}

/// Where the fields of a PackInfo are in a header.
struct PackInfo {
    /// The first packed stream's position (a NUMBER).
    pos: Range<usize>,
    /// The sizes (NUMBERs).
    sizes: Range<usize>,
    /// The digests, where it has them: the bytes after their id.
    crc: Option<Range<usize>>,
    n: usize,
}

/// The PackInfo that starts the streams info at `p` of `h`.
fn pack_info(h: &[u8], mut p: usize) -> Result<PackInfo, String> {
    if number(h, &mut p)? != id::PACK_INFO {
        return Err("no packed streams' info first".into());
    }
    let at = p;
    number(h, &mut p)?;
    let pos = at..p;
    let n = usize::try_from(number(h, &mut p)?).ok().filter(|&n| n <= h.len()).ok_or("a header cut")?;
    let (mut sizes, mut crc) = (p..p, None);
    loop {
        match number(h, &mut p)? {
            id::END => break,
            id::SIZE => {
                let s = p;
                for _ in 0..n {
                    number(h, &mut p)?;
                }
                sizes = s..p;
            }
            id::CRC => {
                let s = p;
                let all = byte(h, &mut p)? != 0;
                let defined = if all {
                    n
                } else {
                    let bits = h.get(p..p + n.div_ceil(8)).ok_or("a header cut")?;
                    p += n.div_ceil(8);
                    (0..n).filter(|&i| bits[i / 8] >> (7 - i % 8) & 1 != 0).count()
                };
                p = p.checked_add(4 * defined).filter(|&p| p <= h.len()).ok_or("a header cut")?;
                crc = Some(s..p);
            }
            _ => return Err("an unknown property in the packed streams' info".into()),
        }
    }
    Ok(PackInfo { pos, sizes, crc, n })
}

/// The digests of `packs` as `old` (the bytes after the id of a CRC property) has them defined.
fn digests(old: &[u8], packs: &[&[u8]]) -> Vec<u8> {
    let all = old[0] != 0;
    let mut v = vec![old[0]];
    let defined: Vec<bool> = if all {
        vec![true; packs.len()]
    } else {
        v.extend(&old[1..1 + packs.len().div_ceil(8)]);
        (0..packs.len()).map(|i| old[1 + i / 8] >> (7 - i % 8) & 1 != 0).collect()
    };
    for (p, _) in packs.iter().zip(&defined).filter(|d| *d.1) {
        v.extend(crc32::hash(p).to_le_bytes());
    }
    v
}

/// `h` with each range replaced (the ranges in order, apart).
fn splice(h: &[u8], edits: Vec<(Range<usize>, Vec<u8>)>) -> Vec<u8> {
    let mut out = Vec::with_capacity(h.len() + 16);
    let mut at = 0;
    for (r, with) in edits {
        out.extend_from_slice(&h[at..r.start]);
        out.extend(with);
        at = r.end;
    }
    out.extend_from_slice(&h[at..]);
    out
}

/// The header `h` with the packed streams' info of its main streams as `sizes` and `packs` tell
/// (its position stays: the streams begin at 0).
fn patch_plain(h: &[u8], packs: &[&[u8]]) -> Result<Vec<u8>, String> {
    let mut p = 0;
    if number(h, &mut p)? != id::HEADER {
        return Err("not a header".into());
    }
    loop {
        match number(h, &mut p)? {
            id::ARCHIVE_PROPERTIES => loop {
                if number(h, &mut p)? == id::END {
                    break;
                }
                let n = number(h, &mut p)?;
                p = usize::try_from(n)
                    .ok()
                    .and_then(|n| p.checked_add(n))
                    .filter(|&p| p <= h.len())
                    .ok_or("a header cut")?;
            },
            id::MAIN_STREAMS => break,
            _ => return Err("a header with additional streams, or no main ones".into()),
        }
    }
    let pi = pack_info(h, p)?;
    if pi.n != packs.len() {
        return Err("the packed streams' info not of the streams".into());
    }
    let mut sizes = Vec::new();
    packs.iter().for_each(|p| put_number(&mut sizes, p.len() as u64));
    let mut edits = vec![(pi.sizes.clone(), sizes)];
    if let Some(c) = pi.crc {
        edits.push((c.clone(), digests(&h[c], packs)));
    }
    Ok(splice(h, edits))
}

/// The encoded header `h` (the streams info of one folder of one coder, one packed stream) as it
/// is where its packed stream, now at `pos`, is `packed` and its output `plain`.
fn patch_encoded(h: &[u8], pos: u64, packed: &[u8], plain: &[u8]) -> Result<Vec<u8>, String> {
    let mut p = 1;
    let pi = pack_info(h, p)?;
    p = pi.crc.as_ref().map_or(pi.sizes.end, |c| c.end);
    // (the end of the packed streams' info)
    if number(h, &mut p)? != id::END || number(h, &mut p)? != id::UNPACK_INFO {
        return Err("an encoded header not as it is written".into());
    }
    let expect = |ok: bool| ok.then_some(()).ok_or_else(|| "an encoded header not as it is written".to_string());
    expect(number(h, &mut p)? == id::FOLDER && number(h, &mut p)? == 1 && byte(h, &mut p)? == 0)?;
    expect(number(h, &mut p)? == 1)?;
    let flags = byte(h, &mut p)?;
    expect(flags & 0xd0 == 0)?;
    p += (flags & 15) as usize;
    if flags & 0x20 != 0 {
        let n = number(h, &mut p)?;
        p += usize::try_from(n).map_err(|_| "a header cut")?;
    }
    expect(number(h, &mut p)? == id::UNPACK_SIZE)?;
    let at = p;
    number(h, &mut p)?;
    let size = at..p;
    let mut edits = vec![(pi.pos.clone(), pos), (pi.sizes.clone(), packed.len() as u64), (size, plain.len() as u64)]
        .into_iter()
        .map(|(r, v)| {
            let mut n = Vec::new();
            put_number(&mut n, v);
            (r, n)
        })
        .collect::<Vec<_>>();
    if let Some(c) = pi.crc {
        edits.push((c.clone(), digests(&h[c], &[packed])));
    }
    // (the folder's CRC-32 after the sizes: the property, defined for the one folder)
    loop {
        match number(h, &mut p)? {
            id::END => break,
            id::CRC => {
                let all = byte(h, &mut p)? != 0;
                let defined = if all { true } else { byte(h, &mut p)? & 0x80 != 0 };
                if defined {
                    edits.push((p..p + 4, crc32::hash(plain).to_le_bytes().to_vec()));
                    p += 4;
                }
            }
            _ => return Err("an encoded header not as it is written".into()),
        }
    }
    edits.sort_by_key(|e| e.0.start);
    Ok(splice(h, edits))
}

/// The coder of the folder that takes its packed stream, where it is one this makes again and
/// every coder is of one input and one output: its index.
fn remade(f: &Folder) -> Option<usize> {
    let [i] = f.packed[..] else { return None };
    let k = usize::try_from(i).ok()?;
    (f.coders.iter().all(|c| c.ins == 1 && c.outs == 1) && f.coders.get(k).is_some_and(supported)).then_some(k)
}

pub(super) fn coding(d: &[u8], _: &Options) -> Result<Option<Vec<u8>>, String> {
    let (span, crc) = header::signature(d).ok_or("not a 7z signature header")?;
    if span.end != d.len() as u64 {
        return Err("data after the header, or a multi-volume archive".into());
    }
    if span.is_empty() {
        return Ok(None);
    }
    let hb = &d[span.start as usize..];
    if crc32::hash(hb) != crc {
        return Err("a header's CRC-32 not right".into());
    }
    let top = header::parse(hb).ok_or("a header not parsed")?;
    // the header, plain: where it was encoded, the coder, its properties and the end of the main streams
    let (plain, coded, end): (header::Header, Option<(Coder, Layer)>, u64) = match top {
        Top::Plain(h) => (h, None, span.start - SIGNATURE as u64),
        Top::Encoded(es) => {
            let ([f], [size]) = (&es.folders[..], &es.pack_sizes[..]) else {
                return Err("an encoded header of more than one stream".into());
            };
            let c = f.single().filter(|c| supported(c)).ok_or("a header of an unknown coder, or encrypted")?;
            let at = SIGNATURE as u64 + es.pack_pos;
            if at.checked_add(*size) != Some(span.start) {
                return Err("an encoded header's stream not before the header".into());
            }
            let packed = &d[at as usize..span.start as usize];
            let layer = decode(c, packed, f.size().ok_or("a header's size not known")?)?;
            if f.crc.is_some_and(|crc| crc != crc32::hash(&layer.data)) {
                return Err("an encoded header's CRC-32 not right".into());
            }
            let Some(Top::Plain(h)) = header::parse(&layer.data) else {
                return Err("an encoded header not parsed".into());
            };
            (h, Some((c.clone(), layer)), es.pack_pos)
        }
    };
    let main = &plain.main;
    if main.folders.is_empty() {
        return Ok(None);
    }
    if main.pack_pos != 0 {
        return Err("packed streams not at the start".into());
    }
    let spans = main.pack_spans().ok_or("packed streams not parsed")?;
    let by_folder = main.folder_packs().ok_or("packed streams not parsed")?;
    if by_folder.last().map_or(0, |r| r.end) != spans.len()
        || spans.last().map_or(0, |r| r.end - SIGNATURE as u64) != end
    {
        return Err("packed streams not following one another".into());
    }
    let old: Vec<&[u8]> = spans.iter().map(|r| &d[r.start as usize..r.end as usize]).collect();
    let mut packs: Vec<Option<Vec<u8>>> = vec![None; old.len()];
    for (f, r) in main.folders.iter().zip(&by_folder) {
        let Some(k) = remade(f) else { continue };
        let c = &f.coders[k];
        let layer = decode(c, old[r.start], f.sizes[k])?;
        let new = remake(c, &layer);
        if new[..] != *old[r.start] {
            packs[r.start] = Some(new);
        }
    }
    if packs.iter().all(Option::is_none) {
        return Ok(None);
    }
    let packs: Vec<&[u8]> = packs.iter().zip(&old).map(|(n, o)| n.as_deref().unwrap_or(o)).collect();
    let total: u64 = packs.iter().map(|p| p.len() as u64).sum();
    // the header with the new sizes, and an encoded one coded again
    let plain_bytes = match &coded {
        None => hb,
        Some((_, l)) => &l.data,
    };
    let patched = patch_plain(plain_bytes, &packs)?;
    let (extra, next) = match coded {
        None => (Vec::new(), patched),
        Some((c, l)) => {
            let packed = remake(&c, &Layer { data: patched.clone(), props: l.props, dict: l.dict });
            let info = patch_encoded(hb, total, &packed, &patched)?;
            (packed, info)
        }
    };
    let mut start = Vec::with_capacity(20);
    start.extend((total + extra.len() as u64).to_le_bytes());
    start.extend((next.len() as u64).to_le_bytes());
    start.extend(crc32::hash(&next).to_le_bytes());
    let mut r = d[..8].to_vec();
    r.extend(crc32::hash(&start).to_le_bytes());
    r.extend(start);
    packs.iter().for_each(|p| r.extend_from_slice(p));
    r.extend(extra);
    r.extend(next);
    Ok(Some(r))
}
