//! PlayStation archives (`PSAR`; PlayStation 3 and 4 games and Sony's tools), big endian.
//! Written from the public descriptions and unverified on real files: no sample was at hand,
//! only archives the tests write.
//!
//! The header (32 bytes): the magic, the version (u16 major, u16 minor), the compression
//! (`zlib` or `lzma`), the TOC's length (u32, counting the header), the size of a TOC entry (30),
//! the count of entries, the size of a block (usually 65536) and the archive flags (u32; 2 the
//! paths are absolute). The TOC: the entries, 30 bytes each (the MD5 of the path, the index of
//! the first block u32, the original size u40, the offset u40), then to the TOC's end the
//! table of the blocks' sizes as they are in the archive (u16 for blocks up to 64 KiB, u24 up
//! to 16 MiB, else u32; 0 stands for a full block, 65536 not fitting in 16 bits).
//!
//! A file's blocks follow one another from its offset, `ceil(size / block size)` of them; one
//! the size of its original bytes (the block size, or the rest for the last) is stored. A file
//! all of whose blocks are stored is a plain file of their extent. A single compressed block of
//! zlib is a plain file of its stream; any file of `lzma` blocks is a file of compressed
//! contents ([`aqr_disk::Compressed::Opaque`]) with its original size, its extent as it is.
//! Other files (several blocks of zlib, not all stored) are left out.
//!
//! The first entry is the manifest: the paths of the others, one on each line, in order, read
//! when its blocks are stored or zlib. Where it cannot be read (`lzma` compressed, for one) the
//! files are named by their index.

use super::{file, file_with, u32be, within, zlib_inflate};
use aqr_disk::{Compressed, File};

pub fn starts(d: &[u8]) -> bool {
    d.starts_with(b"PSAR") && matches!(d.get(8..12), Some(b"zlib" | b"lzma")) && d.len() >= 32
}

fn u40(d: &[u8], at: usize) -> Option<u64> {
    d.get(at..at.checked_add(5)?)?.iter().try_fold(0u64, |a, &b| Some(a << 8 | b as u64))
}

struct Toc<'a> {
    d: &'a [u8],
    block: u64,
    width: usize,
    table: usize,
    blocks: usize,
}

impl Toc<'_> {
    /// How a file's blocks lie: the bytes of its extent in all, and the blocks as (size in the
    /// archive, stored).
    fn blocks(&self, start: u64, size: u64, budget: &mut usize) -> Option<Vec<(u64, bool)>> {
        let n = size.div_ceil(self.block);
        if n > (self.blocks as u64).saturating_sub(start) || n as usize > *budget {
            return None;
        }
        *budget -= n as usize;
        let mut out = Vec::with_capacity(n as usize);
        for k in 0..n {
            let at = self.table + (start + k) as usize * self.width;
            let s = self.d.get(at..at + self.width)?.iter().fold(0u64, |a, &b| a << 8 | b as u64);
            let raw = self.block.min(size - k * self.block);
            let s = if s == 0 { self.block } else { s };
            out.push((s, s == raw));
        }
        Some(out)
    }
}

pub fn files(d: &[u8]) -> Option<Vec<File>> {
    if !starts(d) {
        return None;
    }
    let lzma = &d[8..12] == b"lzma";
    let (toc_len, entry, count, block) =
        (u32be(d, 12)? as usize, u32be(d, 16)? as usize, u32be(d, 20)? as usize, u32be(d, 24)?);
    if entry != 30 || block == 0 || toc_len > d.len() {
        return None;
    }
    let table = 32usize.checked_add(count.checked_mul(30)?)?;
    let width = match block {
        0..=0x10000 => 2,
        0x10001..=0x100_0000 => 3,
        _ => 4,
    };
    let blocks = toc_len.checked_sub(table)? / width;
    let t = Toc { d, block: block as u64, width, table, blocks };
    let mut budget = blocks.saturating_mul(2).saturating_add(count);

    let record = |i: usize| -> Option<(u64, u64, u64)> {
        let at = 32 + i * 30;
        Some((u32be(d, at + 16)? as u64, u40(d, at + 20)?, u40(d, at + 25)?))
    };

    // The manifest.
    let mut names: Vec<Vec<u8>> = Vec::new();
    if count > 0 {
        let (start, size, offset) = record(0)?;
        if let Some(bs) = t.blocks(start, size, &mut budget) {
            let mut text = Vec::new();
            let mut at = offset;
            let mut ok = true;
            for (k, &(s, stored)) in bs.iter().enumerate() {
                let Some(r) = within(d, at, s) else {
                    ok = false;
                    break;
                };
                at += s;
                if stored {
                    text.extend_from_slice(&d[r]);
                } else if lzma {
                    ok = false;
                } else {
                    let raw = block as u64;
                    match zlib_inflate(&d[r], raw.min(size - k as u64 * raw) as usize) {
                        Some(v) => text.extend_from_slice(&v),
                        None => ok = false,
                    }
                }
            }
            if ok {
                let lines: Vec<Vec<u8>> = text
                    .split(|&b| b == b'\n')
                    .map(|l| l.strip_suffix(b"\r").unwrap_or(l))
                    .map(|l| l.strip_prefix(b"/").unwrap_or(l).to_vec())
                    .collect();
                if lines.len() >= count - 1 {
                    names = lines;
                }
            }
        }
    }

    let mut out = Vec::new();
    for i in 1..count {
        let (start, size, offset) = record(i)?;
        let name = names
            .get_mut(i - 1)
            .filter(|n| !n.is_empty())
            .map(std::mem::take)
            .unwrap_or_else(|| format!("{i:06}").into_bytes());
        let Some(bs) = t.blocks(start, size, &mut budget) else { continue };
        let total: u64 = bs.iter().map(|b| b.0).sum();
        let Some(r) = within(d, offset, total) else { continue };
        if r.is_empty() {
            continue;
        }
        if bs.iter().all(|b| b.1) {
            out.push(file(name, r));
        } else if lzma {
            out.push(file_with(name, r, Some(Compressed::Opaque { len: size })));
        } else if bs.len() == 1 {
            out.push(file(name, r));
        }
    }
    Some(out)
}
