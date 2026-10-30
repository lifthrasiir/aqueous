//! The `.xz` container: streams, each a header, blocks (a header, the output of its filters,
//! padding and a check of its data), an index of the blocks and a footer, with zeros padding
//! the streams. The blocks whose last filter is LZMA2 are taken.

use std::ops::Range;

use aqr_common::{crc32, varint};

use crate::lzma2;

pub const MAGIC: [u8; 6] = [0xfd, b'7', b'z', b'X', b'Z', 0];
const FOOTER_MAGIC: [u8; 2] = *b"YZ";
const HEADER: usize = 12;

/// A block ending with LZMA2: its LZMA2 stream and dictionary size, and the filters before it
/// (ids and properties, the first applied first in encoding).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    pub stream: Range<usize>,
    pub dict: u32,
    pub filters: Vec<(u64, Vec<u8>)>,
}

/// The streams at the start of a file: their blocks ending with LZMA2, in order, and their end
/// (with the padding after the last, where the file ends or another stream follows it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Streams {
    pub blocks: Vec<Block>,
    pub end: usize,
}

/// The bytes of the check of each type.
const CHECK: [usize; 16] = [0, 4, 4, 4, 8, 8, 8, 16, 16, 16, 32, 32, 32, 64, 64, 64];

fn le32(f: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(f.get(at..at + 4)?.try_into().unwrap()))
}

/// Whether `d` starts with a stream header (its magic, flags and their CRC-32).
pub fn starts(d: &[u8]) -> bool {
    d.len() >= HEADER && d[..6] == MAGIC && d[6] == 0 && d[7] < 16 && le32(d, 8) == Some(crc32::hash(&d[6..8]))
}

/// The bytes of the LZMA2 stream at the start of `d`, found by its chunks' headers alone.
pub fn lzma2_extent(d: &[u8]) -> Option<usize> {
    let mut at = 0;
    loop {
        let c = *d.get(at)?;
        at += match c {
            0 => return Some(at + 1),
            1 | 2 => 3 + (u16::from_be_bytes(d.get(at + 1..at + 3)?.try_into().unwrap()) as usize + 1),
            0x80.. => {
                let packed = u16::from_be_bytes(d.get(at + 3..at + 5)?.try_into().unwrap()) as usize + 1;
                (if c >= 0xc0 { 6 } else { 5 }) + packed
            }
            _ => return None,
        };
    }
}

/// The block at `at` (of a stream of checks of `check` bytes): the block of LZMA2 alone, if it
/// is one, and the end of the block.
fn block(f: &[u8], at: usize, check: usize) -> Option<(Option<Block>, usize)> {
    let size = (*f.get(at)? as usize + 1) * 4;
    let h = f.get(at..at + size)?;
    if le32(h, size - 4)? != crc32::hash(&h[..size - 4]) || h[1] & 0x3c != 0 {
        return None;
    }
    let mut p = 2;
    let get = |p: &mut usize| varint::get(&h[..size - 4], p).ok();
    let packed = if h[1] & 0x40 != 0 { Some(get(&mut p)?) } else { None };
    if h[1] & 0x80 != 0 {
        get(&mut p)?;
    }
    let mut filters = Vec::new();
    for _ in 0..(h[1] & 3) + 1 {
        let id = get(&mut p)?;
        let n = get(&mut p)? as usize;
        filters.push((id, h.get(p..p.checked_add(n)?)?));
        p += n;
    }
    let start = at + size;
    // (the output of a chain ending with LZMA2 is an LZMA2 stream)
    let stream = match (filters.last()?, packed) {
        (&(0x21, _), _) => start + lzma2_extent(&f[start..])?,
        (_, Some(n)) => start.checked_add(usize::try_from(n).ok()?)?,
        _ => return None,
    };
    let lzma2 = match filters.split_last() {
        Some((&(0x21, &[b]), rest)) if rest.iter().all(|f| f.0 != 0x21) => lzma2::dict_size(b).map(|dict| Block {
            stream: start..stream,
            dict,
            filters: rest.iter().map(|&(id, p)| (id, p.to_vec())).collect(),
        }),
        _ => None,
    };
    let pad = (4 - (stream - at) % 4) % 4;
    if f.get(stream..stream + pad)?.iter().any(|&b| b != 0) {
        return None;
    }
    let end = stream + pad + check;
    (end <= f.len()).then_some((lzma2, end))
}

/// The end of the index at `at` (its indicator byte read), of `n` blocks.
fn index(f: &[u8], at: usize, n: usize) -> Option<usize> {
    let mut p = at + 1;
    let get = |p: &mut usize| varint::get(f, p).ok();
    if get(&mut p)? != n as u64 {
        return None;
    }
    for _ in 0..2 * n {
        get(&mut p)?;
    }
    let pad = (4 - (p - at) % 4) % 4;
    if f.get(p..p + pad)?.iter().any(|&b| b != 0) || le32(f, p + pad)? != crc32::hash(&f[at..p + pad]) {
        return None;
    }
    Some(p + pad + 4)
}

/// The streams at the start of `f`. None where it does not start with one, or the first is
/// damaged (those after it are left out where damaged).
pub fn parse(f: &[u8]) -> Option<Streams> {
    let mut out = Streams { blocks: Vec::new(), end: 0 };
    let mut at = 0;
    while starts(&f[at..]) {
        let check = CHECK[f[at + 7] as usize];
        let flags = at + 6;
        let mut blocks = Vec::new();
        let mut p = at + HEADER;
        let mut n = 0;
        let end = loop {
            match f.get(p) {
                None => break None,
                Some(0) => break index(f, p, n),
                _ => {}
            }
            let Some((b, end)) = block(f, p, check) else { break None };
            blocks.extend(b);
            n += 1;
            p = end;
        };
        // the footer: its CRC-32, the index's size and the stream's flags, then its magic
        let Some(e) = end.filter(|&e| {
            f.get(e..e + 12).is_some_and(|t| {
                t[10..] == FOOTER_MAGIC
                    && t[8..10] == f[flags..flags + 2]
                    && le32(t, 0) == Some(crc32::hash(&t[4..10]))
                    && (le32(t, 4).unwrap() as usize + 1) * 4 == e - p
            })
        }) else {
            break;
        };
        out.blocks.extend(blocks);
        at = e + 12;
        out.end = at;
        // padding: zeros in fours, before another stream or the end
        let z = f[at..].iter().take_while(|&&b| b == 0).count() / 4 * 4;
        if at + z == f.len() || starts(&f[at + z..]) {
            at += z;
            out.end = at;
        }
    }
    (out.end > 0).then_some(out)
}
