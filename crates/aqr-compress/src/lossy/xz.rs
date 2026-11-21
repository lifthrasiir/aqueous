//! `.xz` files: `coding`, the LZMA2 stream of each block made again by [`aqr_lzma::canonical`]
//! (the filters before it, BCJ or Delta, apply to the data as it is: the stream's data is the
//! filtered data), whose record by aqr-lzma is a few percent of XZ Utils'. The block headers
//! (the filter flags, the LZMA2 dictionary size among them, and the uncompressed size, kept;
//! the compressed size, where there is one, and the CRC-32 written again), the checks (those
//! of the data, which does not change), the indexes (the unpadded sizes written again), the
//! footers, the padding of blocks and between streams are laid out again as they were. A file of
//! anything else than streams one after another, a block of a chain not ending with LZMA2 and an
//! index not telling its blocks are kept as they are.

use aqr_common::{crc32, varint};
use aqr_lzma::lzma::Props;
use aqr_lzma::{canonical, lzma2};

use crate::Options;

/// The most bytes of data of a block made again (the same as `lossy:max-data`'s default).
const MAX_DATA: usize = 1 << 30;
const MAGIC: [u8; 6] = [0xfd, b'7', b'z', b'X', b'Z', 0];
/// The bytes of the check of each type.
const CHECK: [usize; 16] = [0, 4, 4, 4, 8, 8, 8, 16, 16, 16, 32, 32, 32, 64, 64, 64];
/// LZMA2's properties where its first LZMA chunk gives none (all of its chunks stored).
const DEFAULT_PROPS: Props = Props { lc: 3, lp: 0, pb: 2 };

fn le32(d: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(d.get(at..at.checked_add(4)?)?.try_into().unwrap()))
}

fn get(d: &[u8], p: &mut usize) -> Result<u64, String> {
    varint::get(d, p).map_err(|_| "a varint damaged".to_string())
}

fn pad4(v: &mut Vec<u8>, from: usize) {
    while !(v.len() - from).is_multiple_of(4) {
        v.push(0);
    }
}

/// A block header, parsed.
struct Header {
    size: usize,
    /// The uncompressed size field, as it is (the varint).
    uncompressed: Option<Vec<u8>>,
    packed: Option<u64>,
    /// The filter flags, as they are.
    filters: Vec<u8>,
    /// The last filter's properties byte, where the chain ends with LZMA2.
    dict_byte: Option<u8>,
}

fn header(d: &[u8], at: usize) -> Result<Header, String> {
    let size = (*d.get(at).ok_or("a block header cut")? as usize + 1) * 4;
    let h = d.get(at..at + size).ok_or("a block header cut")?;
    if le32(h, size - 4) != Some(crc32::hash(&h[..size - 4])) {
        return Err("a block header's CRC-32 not right".into());
    }
    if h[1] & 0x3c != 0 {
        return Err("a block header with reserved flags".into());
    }
    let h = &h[..size - 4];
    let mut p = 2;
    let packed = if h[1] & 0x40 != 0 { Some(get(h, &mut p)?) } else { None };
    let uncompressed = if h[1] & 0x80 != 0 {
        let s = p;
        get(h, &mut p)?;
        Some(h[s..p].to_vec())
    } else {
        None
    };
    let from = p;
    let mut last = None;
    for _ in 0..(h[1] & 3) + 1 {
        let id = get(h, &mut p)?;
        let n = get(h, &mut p)? as usize;
        let props =
            h.get(p..p.checked_add(n).ok_or("a filter's properties cut")?).ok_or("a filter's properties cut")?;
        last = Some((id, props));
        p += n;
    }
    // (what is left is zeros)
    if h[p..].iter().any(|&b| b != 0) {
        return Err("a block header's padding not zeros".into());
    }
    let filters = h[from..p].to_vec();
    match last {
        Some((0x21, &[b])) if lzma2::dict_size(b).is_some() => {
            Ok(Header { size, uncompressed, packed, filters, dict_byte: Some(b) })
        }
        _ => Err("a block of a filter chain not ending with LZMA2".into()),
    }
}

/// A block header of the fields kept, the compressed size `packed` written (if the header has one).
fn write_header(h: &Header, packed: Option<u64>) -> Vec<u8> {
    let mut v = vec![0, (h.filters_count() - 1) as u8];
    if let Some(n) = packed {
        v[1] |= 0x40;
        varint::put(&mut v, n);
    }
    if let Some(u) = &h.uncompressed {
        v[1] |= 0x80;
        v.extend_from_slice(u);
    }
    v.extend_from_slice(&h.filters);
    // (the CRC-32 comes after the padding, in the last 4 bytes)
    while !v.len().is_multiple_of(4) {
        v.push(0);
    }
    v.extend([0; 4]);
    v[0] = (v.len() / 4 - 1) as u8;
    let n = v.len();
    let crc = crc32::hash(&v[..n - 4]);
    v[n - 4..].copy_from_slice(&crc.to_le_bytes());
    v
}

impl Header {
    fn filters_count(&self) -> usize {
        let (mut p, mut n) = (0, 0);
        while p < self.filters.len() {
            let _ = varint::get(&self.filters, &mut p);
            let len = varint::get(&self.filters, &mut p).unwrap_or(0) as usize;
            p += len;
            n += 1;
        }
        n
    }
}

pub(super) fn coding(d: &[u8], _: &Options) -> Result<Option<Vec<u8>>, String> {
    let mut r = Vec::with_capacity(d.len());
    let mut p = 0;
    while p < d.len() {
        // a stream: its header
        let sh = d.get(p..p + 12).ok_or("data after the streams")?;
        if sh[..6] != MAGIC || sh[6] != 0 || sh[7] >= 16 || le32(sh, 8) != Some(crc32::hash(&sh[6..8])) {
            return Err("data after the streams".into());
        }
        let check = CHECK[sh[7] as usize];
        r.extend_from_slice(sh);
        let mut at = p + 12;
        // its blocks: the unpadded sizes and the uncompressed sizes of the index to be
        let mut records: Vec<(u64, u64)> = Vec::new();
        let mut old: Vec<(u64, u64)> = Vec::new();
        while d.get(at).ok_or("a stream cut")? != &0 {
            let h = header(d, at)?;
            let start = at + h.size;
            let body = d.get(start..).ok_or("a block cut")?;
            let dec = lzma2::decode(body, MAX_DATA).map_err(|_| "a block not decoded, or over 1 GiB of data")?;
            let used = dec.used;
            let size = dec.segments.iter().map(|s| s.data.len()).sum::<usize>();
            if size >= MAX_DATA {
                return Err("over 1 GiB of data in a block".into());
            }
            if h.packed.is_some_and(|n| n != used as u64) {
                return Err("a block's compressed size not its stream's".into());
            }
            if let Some(u) = &h.uncompressed
                && varint::get(u, &mut 0) != Ok(size as u64)
            {
                return Err("a block's uncompressed size not its data's".into());
            }
            let props = dec.segments.iter().flat_map(|s| &s.chunks).find_map(|c| c.props).unwrap_or(DEFAULT_PROPS);
            let dict = lzma2::dict_size(h.dict_byte.expect("LZMA2")).expect("a dictionary size");
            let mut data = Vec::with_capacity(size);
            for s in &dec.segments {
                data.extend_from_slice(&s.data);
            }
            drop(dec);
            aqr_common::allowance::check();
            let stream = canonical::lzma2(&data, props, dict);
            aqr_common::allowance::check();
            drop(data);
            let hdr = write_header(&h, h.packed.map(|_| stream.len() as u64));
            // the block as it was: its padding (zeros) and check
            let after = start + used;
            let pad = (4 - (h.size + used) % 4) % 4;
            if d.get(after..after + pad).ok_or("a block cut")?.iter().any(|&b| b != 0) {
                return Err("a block's padding not zeros".into());
            }
            let chk = d.get(after + pad..after + pad + check).ok_or("a block cut")?;
            old.push(((h.size + used + check) as u64, size as u64));
            records.push(((hdr.len() + stream.len() + check) as u64, size as u64));
            let block = r.len();
            r.extend_from_slice(&hdr);
            r.extend_from_slice(&stream);
            pad4(&mut r, block);
            r.extend_from_slice(chk);
            at = after + pad + check;
        }
        // its index, which must tell the blocks as they were
        let idx = at;
        let mut q = at + 1;
        let n = get(d, &mut q)?;
        if n != old.len() as u64 {
            return Err("an index not telling the blocks".into());
        }
        for rec in &old {
            let (u, s) = (get(d, &mut q)?, get(d, &mut q)?);
            if (u, s) != *rec {
                return Err("an index not telling the blocks".into());
            }
        }
        let pad = (4 - (q - idx) % 4) % 4;
        let end = q + pad + 4;
        if d.get(q..q + pad).ok_or("an index cut")?.iter().any(|&b| b != 0)
            || le32(d, q + pad) != Some(crc32::hash(&d[idx..q + pad]))
        {
            return Err("an index damaged".into());
        }
        let mut index = vec![0];
        varint::put(&mut index, records.len() as u64);
        for &(u, s) in &records {
            varint::put(&mut index, u);
            varint::put(&mut index, s);
        }
        pad4(&mut index, 0);
        let crc = crc32::hash(&index);
        index.extend(crc.to_le_bytes());
        // its footer
        let f = d.get(end..end + 12).ok_or("a footer cut")?;
        if f[10..] != *b"YZ"
            || f[8..10] != sh[6..8]
            || le32(f, 0) != Some(crc32::hash(&f[4..10]))
            || (le32(f, 4).unwrap() as usize + 1) * 4 != end - idx
        {
            return Err("a footer damaged".into());
        }
        let mut footer = Vec::with_capacity(12);
        footer.extend_from_slice(&((index.len() / 4 - 1) as u32).to_le_bytes());
        footer.extend_from_slice(&sh[6..8]);
        let crc = crc32::hash(&footer);
        r.extend_from_slice(&index);
        r.extend_from_slice(&crc.to_le_bytes());
        r.extend_from_slice(&footer);
        r.extend_from_slice(b"YZ");
        // the padding after it: zeros in fours, as they are, before another stream or the end
        p = end + 12;
        let z = d[p..].iter().take_while(|&&b| b == 0).count();
        if z % 4 != 0 {
            return Err("data after the streams".into());
        }
        r.extend_from_slice(&d[p..p + z]);
        p += z;
    }
    Ok((r[..] != d[..]).then_some(r))
}
