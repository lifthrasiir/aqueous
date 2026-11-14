//! A compressor's stream decoded in order, its data given in chunks as it is (so that only so
//! much of it is held at once): gzip (members one after another), bzip2 (streams one after
//! another, a wave of blocks at a time on the pool's threads), xz (streams of blocks, a wave of
//! blocks at a time), LZMA (`.lzma`, whole) and Zstandard (frames); or a stream of no
//! compressor (a tar archive alone).

use std::ops::Range;

use aqr_bcj::formula::Filter as Bcj;
use aqr_common::{crc32, pool, varint};
use aqr_formula::aqr::{Reached, Step};

use crate::Filter;
use crate::codec::{self, CHUNK, Crc64};

/// What is given the data: a vector whose bytes from the index given on are new (those before
/// are not); false stops the decoding.
pub type Give<'g> = dyn FnMut(Vec<u8>, usize) -> bool + 'g;

/// The data of `d` (a stream of `f`, or data of no compressor) given to `give` in order.
/// Returns whether all was given (false: `give` stopped it).
pub fn decode(f: Option<Filter>, d: &[u8], threads: usize, give: &mut Give) -> Result<bool, String> {
    match f {
        None => Ok(d.chunks(CHUNK).all(|c| give(c.to_vec(), 0))),
        Some(Filter::Gzip) => gzip(d, give),
        Some(Filter::Bzip2) => bzip2(d, threads, give),
        Some(Filter::Xz) => xz(d, threads, give),
        Some(Filter::Lzma) => {
            const HEADER: usize = 13;
            let h = d.get(..HEADER).ok_or("LZMA: no header")?;
            let size = u64::from_le_bytes(h[5..13].try_into().unwrap());
            let (data, _) = codec::lzma(&d[HEADER..], h[0], (size != u64::MAX).then_some(size), isize::MAX as usize)?;
            Ok(data.is_empty() || give(data, 0))
        }
        Some(Filter::Zstd) => {
            let mut stopped = false;
            codec::zstd_chunks(d, &mut |c| {
                stopped = !give(c, 0);
                !stopped
            })?;
            Ok(!stopped)
        }
    }
}

/// The bytes of data `d` holds, as far as it tells (0: not known): of gzip, its last member's
/// size (of the size mod 2^32 at least that of the stream); of xz, its index's.
pub fn estimate(f: Option<Filter>, d: &[u8]) -> u64 {
    match f {
        None => d.len() as u64,
        Some(Filter::Gzip) if d.len() >= 18 => {
            let n = u32::from_le_bytes(d[d.len() - 4..].try_into().unwrap()) as u64;
            let len = d.len() as u64;
            if n >= len { n } else { n + (len - n).div_ceil(1 << 32) * (1 << 32) }
        }
        Some(Filter::Xz) => xz_blocks(d).map_or(0, |(_, n)| n),
        Some(Filter::Lzma) if d.len() >= 13 => match u64::from_le_bytes(d[5..13].try_into().unwrap()) {
            u64::MAX => 0,
            n => n,
        },
        _ => 0,
    }
}

/// How far decoding is, of `n` bytes of data made of `total` (0: not known) from a stream of
/// `packed` bytes.
pub fn reached(n: u64, total: u64, packed: u64) -> Step {
    let total = total.max(n);
    let done = if total == 0 { packed } else { (packed as f64 * n as f64 / total as f64) as u64 };
    Step::Reached(Reached { decompressed: (done.min(packed), packed), evaluated: (n, total) })
}

/// The bytes of the gzip member header at the start of `d`.
fn gzip_header(d: &[u8]) -> Option<usize> {
    if d.len() < 10 || d[..3] != [0x1f, 0x8b, 8] || d[3] & 0xe0 != 0 {
        return None;
    }
    let flags = d[3];
    let mut p = 10;
    if flags & 4 != 0 {
        p += 2 + u16::from_le_bytes(d.get(p..p + 2)?.try_into().unwrap()) as usize;
    }
    for f in [8, 16] {
        if flags & f != 0 {
            p += d.get(p..)?.iter().position(|&b| b == 0)? + 1;
        }
    }
    if flags & 2 != 0 {
        p += 2;
    }
    (p <= d.len()).then_some(p)
}

fn gzip(d: &[u8], give: &mut Give) -> Result<bool, String> {
    let mut at = 0;
    loop {
        let h = gzip_header(&d[at..]).ok_or("gzip: not a stream")?;
        let (mut crc, mut size) = (crc32::Hasher::new(), 0u64);
        let used = codec::inflate_chunks(&d[at + h..], &mut |v, s| {
            crc.update(&v[s..]);
            size += (v.len() - s) as u64;
            give(v, s)
        })?;
        let Some(used) = used else { return Ok(false) };
        at += h + used;
        let t = d.get(at..at + 8).ok_or("gzip: cut short")?;
        if u32::from_le_bytes(t[..4].try_into().unwrap()) != crc.finalize() {
            return Err("gzip: CRC mismatch".into());
        }
        if u32::from_le_bytes(t[4..].try_into().unwrap()) != size as u32 {
            return Err("gzip: size mismatch".into());
        }
        at += 8;
        // (another member, or zeros padding it to the end)
        if d[at..].iter().all(|&b| b == 0) {
            return Ok(true);
        }
        if gzip_header(&d[at..]).is_none() {
            return Err("gzip: data after the stream".into());
        }
    }
}

fn bzip2(d: &[u8], threads: usize, give: &mut Give) -> Result<bool, String> {
    let mut at = 0;
    loop {
        let used = aqr_bz2::stream::decode_chunks(&d[at..], threads, &mut |v| give(v, 0))
            .map_err(|e| format!("bzip2: {e}"))?;
        let Some(used) = used else { return Ok(false) };
        at += used;
        if d[at..].iter().all(|&b| b == 0) {
            return Ok(true);
        }
        if aqr_bz2::stream::level(&d[at..]).is_none() {
            return Err("bzip2: data after the stream".into());
        }
    }
}

/// A block of an `.xz` stream: where its header starts, its filters before LZMA2 (the first
/// applied first in encoding), its LZMA2 stream, its size where told, and its check (type,
/// bytes).
struct XzBlock {
    header: usize,
    filters: Vec<Bcj>,
    lzma2: Range<usize>,
    size: Option<u64>,
    check: (u8, Range<usize>),
}

const XZ_MAGIC: [u8; 6] = [0xfd, b'7', b'z', b'X', b'Z', 0];
/// The bytes of the check of each type.
const XZ_CHECK: [usize; 16] = [0, 4, 4, 4, 8, 8, 8, 16, 16, 16, 32, 32, 32, 64, 64, 64];

fn le32(d: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(d.get(at..at + 4)?.try_into().unwrap()))
}

/// The blocks of the streams of `d`, and the bytes of their data as their indexes tell.
fn xz_blocks(d: &[u8]) -> Result<(Vec<XzBlock>, u64), String> {
    let bad = |what: &str| format!("xz: {what}");
    let (mut blocks, mut total) = (Vec::new(), 0u64);
    let mut at = 0;
    loop {
        let h = d.get(at..at + 12).ok_or_else(|| bad("not a stream"))?;
        if h[..6] != XZ_MAGIC || le32(h, 8) != Some(crc32::hash(&h[6..8])) || h[6] != 0 || h[7] > 15 {
            return Err(bad("not a stream"));
        }
        let (flags, check) = ([h[6], h[7]], h[7]);
        let check_len = XZ_CHECK[check as usize];
        let mut p = at + 12;
        let mut n = 0u64;
        // the blocks, up to the index
        while *d.get(p).ok_or_else(|| bad("cut short"))? != 0 {
            let size = (d[p] as usize + 1) * 4;
            let hb = d.get(p..p + size).ok_or_else(|| bad("cut short"))?;
            if le32(hb, size - 4) != Some(crc32::hash(&hb[..size - 4])) || hb[1] & 0x3c != 0 {
                return Err(bad("a damaged block header"));
            }
            let fields = &hb[..size - 4];
            let mut q = 2;
            let get = |q: &mut usize| varint::get(fields, q).map_err(|_| bad("a damaged block header"));
            let packed = if hb[1] & 0x40 != 0 { Some(get(&mut q)?) } else { None };
            let unpacked = if hb[1] & 0x80 != 0 { Some(get(&mut q)?) } else { None };
            let mut chain = Vec::new();
            for _ in 0..(hb[1] & 3) + 1 {
                let id = get(&mut q)?;
                let len = get(&mut q)? as usize;
                let props = q.checked_add(len).and_then(|e| fields.get(q..e)).ok_or_else(|| bad("a damaged block header"))?;
                q += len;
                chain.push((id, props));
            }
            let Some((&(0x21, _), before)) = chain.split_last() else {
                return Err(bad("a block not of LZMA2 (filters not known)"));
            };
            let filters = before
                .iter()
                .map(|&(id, props)| Bcj::from_xz(id, props).ok_or_else(|| bad(&format!("filter {id:#x} not known"))))
                .collect::<Result<Vec<_>, _>>()?;
            let start = p + size;
            let end = match packed {
                Some(n) => start.checked_add(usize::try_from(n).map_err(|_| bad("too large"))?),
                None => aqr_lzma::xz::lzma2_extent(&d[start..]).map(|n| start + n),
            }
            .filter(|&e| e <= d.len())
            .ok_or_else(|| bad("cut short"))?;
            let pad = (4 - (end - p) % 4) % 4;
            if d.get(end..end + pad).is_none_or(|z| z.iter().any(|&b| b != 0)) {
                return Err(bad("a damaged block"));
            }
            let c = end + pad;
            if c + check_len > d.len() {
                return Err(bad("cut short"));
            }
            blocks.push(XzBlock {
                header: p,
                filters,
                lzma2: start..end,
                size: unpacked,
                check: (check, c..c + check_len),
            });
            n += 1;
            p = c + check_len;
        }
        // the index: its records (the blocks' sizes), checked against the blocks
        let index = p;
        let mut q = p + 1;
        let get = |q: &mut usize| varint::get(d, q).map_err(|_| bad("a damaged index"));
        if get(&mut q)? != n {
            return Err(bad("an index not of its blocks"));
        }
        let first = blocks.len() - n as usize;
        for b in &mut blocks[first..] {
            let unpadded = get(&mut q)?;
            let size = get(&mut q)?;
            if unpadded != (b.lzma2.end - b.header + b.check.1.len()) as u64 || b.size.is_some_and(|s| s != size) {
                return Err(bad("an index not of its blocks"));
            }
            b.size = Some(size);
            total += size;
        }
        let pad = (4 - (q - index) % 4) % 4;
        if d.get(q..q + pad).is_none_or(|z| z.iter().any(|&b| b != 0))
            || le32(d, q + pad) != Some(crc32::hash(&d[index..q + pad]))
        {
            return Err(bad("a damaged index"));
        }
        let footer = q + pad + 4;
        let f = d.get(footer..footer + 12).ok_or_else(|| bad("cut short"))?;
        if f[10..] != *b"YZ" || f[8..10] != flags || le32(f, 0) != Some(crc32::hash(&f[4..10])) {
            return Err(bad("a damaged footer"));
        }
        at = footer + 12;
        // padding of zeros in fours, then another stream or the end
        let z = d[at..].iter().take_while(|&&b| b == 0).count();
        if at + z == d.len() && z % 4 == 0 {
            return Ok((blocks, total));
        }
        if z % 4 != 0 {
            return Err(bad("data after the stream"));
        }
        at += z;
    }
}

/// The data of block `b` of `d`, its check checked (but SHA-256's).
fn xz_block(d: &[u8], b: &XzBlock, threads: usize) -> Result<Vec<u8>, String> {
    let limit = b.size.map_or(isize::MAX as usize, |s| s as usize);
    let (mut out, used) = codec::lzma2_par(&d[b.lzma2.clone()], limit, threads).map_err(|e| format!("xz: {e}"))?;
    if used != b.lzma2.len() || b.size.is_some_and(|s| s != out.len() as u64) {
        return Err("xz: a block not of its size".into());
    }
    b.filters.iter().rev().for_each(|f| f.decode(&mut out));
    let want = &d[b.check.1.clone()];
    let ok = match b.check.0 {
        1 => crc32::hash(&out).to_le_bytes() == want,
        4 => {
            let mut c = Crc64::new();
            c.update(&out);
            c.finish().to_le_bytes() == want
        }
        _ => true,
    };
    if !ok {
        return Err("xz: a check mismatch".into());
    }
    Ok(out)
}

fn xz(d: &[u8], threads: usize, give: &mut Give) -> Result<bool, String> {
    let (blocks, _) = xz_blocks(d)?;
    // a wave of blocks at a time, so that what is held is a few blocks per thread
    for wave in blocks.chunks(threads.max(1) * 2) {
        // (a block alone decoded on the threads by the segments of its LZMA2 stream, if any)
        let inner = if wave.len() == 1 { threads } else { 1 };
        let made = pool::par_map(wave.len(), threads, |k| xz_block(d, &wave[k], inner));
        for m in made {
            let m = m?;
            if !m.is_empty() && !give(m, 0) {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gzip_headers() {
        assert_eq!(gzip_header(&[0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 3]), Some(10));
        assert_eq!(gzip_header(&[0x1f, 0x8b, 8, 8, 0, 0, 0, 0, 0, 3, b'a', 0]), Some(12));
        assert_eq!(gzip_header(&[0x1f, 0x8b, 8, 8, 0, 0, 0, 0, 0, 3, b'a']), None);
        assert_eq!(gzip_header(&[0x1f, 0x8b, 9, 0, 0, 0, 0, 0, 0, 3]), None);
    }
}
