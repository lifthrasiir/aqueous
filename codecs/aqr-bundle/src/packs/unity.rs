//! Unity's AssetBundles of the format `UnityFS` (versions 6 to 8; Unity 5.3 on).
//!
//! The header, big endian: `UnityFS` and a NUL, the format's version (u32), the engine's version
//! and revision (strings of a NUL), the bundle's size (u64), the sizes of the blocks' table
//! compressed and not (u32 each) and the flags (u32: the table's compression in the low 6 bits,
//! 0x40 the table and the directory together, 0x80 the table at the end of the bundle, 0x200
//! the blocks after the table padded to 16 bytes). From version 7 the header is padded to 16
//! bytes. The table: a hash (16 bytes), the blocks (a count, u32; each its size not compressed
//! and compressed, u32, and flags, u16: the compression in the low 6 bits), then the nodes (a
//! count, u32; each its offset and size in the blocks' data decompressed, u64, flags, u32, and
//! path, a string). The blocks follow one another.
//!
//! The table is compressed by LZ4 (as Unity writes it) or not; a table of LZMA is not read. A
//! node is taken where all of it lies in blocks stored as they are (compression 0); those in
//! compressed blocks (LZ4, LZMA) are the rest.

use super::{cstr, file, u32be, u64be};
use aqr_disk::File;

const MAGIC: &[u8; 8] = b"UnityFS\0";
const AT_END: u32 = 0x80;
const PAD_BLOCKS: u32 = 0x200;

pub fn starts(d: &[u8]) -> bool {
    d.starts_with(MAGIC) && u32be(d, 8).is_some_and(|v| (6..=8).contains(&v))
}

/// The table of blocks of bundle `d` decompressed, and where its blocks start.
fn table(d: &[u8]) -> Option<(Vec<u8>, usize)> {
    let version = u32be(d, 8)?;
    let (_, at) = cstr(d, 12)?;
    let (_, mut at) = cstr(d, at)?;
    let size = u64be(d, at)?;
    let (packed, unpacked, flags) = (u32be(d, at + 8)? as usize, u32be(d, at + 12)? as usize, u32be(d, at + 16)?);
    at += 20;
    if size > d.len() as u64 {
        return None;
    }
    if version >= 7 {
        at = at.next_multiple_of(16);
    }
    let (raw, mut blocks) = match flags & AT_END {
        0 => (d.get(at..at.checked_add(packed)?)?, at + packed),
        _ => (d.get((size as usize).checked_sub(packed)?..size as usize)?, at),
    };
    if flags & PAD_BLOCKS != 0 {
        blocks = blocks.next_multiple_of(16);
    }
    let table = match flags & 0x3f {
        0 if packed == unpacked => raw.to_vec(),
        2 | 3 => {
            let mut out = Vec::with_capacity(unpacked);
            aqr_lz4::block::decode(raw, &mut out, 0, unpacked).ok()?;
            (out.len() == unpacked).then_some(out)?
        }
        _ => return None,
    };
    Some((table, blocks))
}

/// The nodes of bundle `d` that lie in blocks stored as they are; None where it is no bundle,
/// or its table is not read.
pub fn files(d: &[u8]) -> Option<Vec<File>> {
    if !starts(d) {
        return None;
    }
    let (t, start) = table(d)?;
    let n = u32be(&t, 16)? as usize;
    if n > t.len() / 10 {
        return None;
    }
    // the blocks: where each starts in the data decompressed and in `d`, its size, and whether
    // it is stored as it is
    let (mut at, mut data, mut pos) = (20, 0u64, start as u64);
    let mut blocks = Vec::with_capacity(n);
    for _ in 0..n {
        let (size, packed) = (u32be(&t, at)? as u64, u32be(&t, at + 4)? as u64);
        let flags = u16::from_be_bytes(t.get(at + 8..at + 10)?.try_into().ok()?);
        let stored = flags & 0x3f == 0 && size == packed;
        blocks.push((data, pos, size, stored));
        (data, pos, at) = (data + size, pos + packed, at + 10);
    }
    if pos > d.len() as u64 {
        return None;
    }
    let nodes = u32be(&t, at)? as usize;
    at += 4;
    if nodes > t.len().saturating_sub(at) / 21 {
        return None;
    }
    let mut files = Vec::with_capacity(nodes);
    for _ in 0..nodes {
        let (offset, size) = (u64be(&t, at)?, u64be(&t, at + 8)?);
        let (path, next) = cstr(&t, at + 20)?;
        at = next;
        let end = offset.checked_add(size)?;
        if size == 0 || end > data {
            continue;
        }
        // (the blocks it lies in, all stored: then contiguous in `d`)
        let first = blocks.partition_point(|b| b.0 + b.2 <= offset);
        let last = blocks.partition_point(|b| b.0 < end);
        if blocks[first..last].iter().all(|b| b.3) {
            let b = blocks[first];
            let s = (b.1 + offset - b.0) as usize;
            files.push(file(path.to_vec(), s..s + size as usize));
        }
    }
    Some(files)
}
