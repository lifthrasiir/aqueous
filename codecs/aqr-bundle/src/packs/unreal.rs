//! Unreal Engine 4 and 5's `.pak` files, versions 1 to 11 (UE 4.0 to 5.x), little endian.
//!
//! The footer (`FPakInfo`) ends the file: from version 7 an encryption key's GUID (16 bytes),
//! whether the index is encrypted (u8), the magic `E1 12 6F 5A`, the version (u32), the index's
//! offset and size (u64 each) and SHA-1 (20 bytes), then from version 8 the names of the
//! compression methods (32 bytes each, 4 or 5 of them) and in version 9 whether the index is
//! frozen (u8). The index starts with the mount point (an `FString`: an i32 length, then that
//! many bytes, a NUL last; a negative length counts UTF-16 units) and the count of entries (i32).
//!
//! Before version 10 each entry follows: its file name (an `FString`) and an `FPakEntry`: the
//! offset of its copy before the data, the size compressed and not (u64 each), the
//! compression method (u32: before version 8 1 zlib, 2 gzip, 4 a plugin's; from 8 the index of
//! a name of the footer, 0 none), in version 1 a time (u64), the data's SHA-1 (20 bytes), and
//! from version 3, where compressed, the blocks (a count, i32, and each block's start and end,
//! u64), the flags (u8: 1 encrypted, 2 deleted) and the block size (u32). From version 10
//! (UE 4.26 on) the entries are encoded in bits (`FPakFile::EncodePakEntry`: a u32 of flags
//! and sizes, then the offset and sizes as u32 or u64, the blocks' sizes), those that do not
//! fit kept as `FPakEntry`s after them, and their names are in the full directory index, at
//! an offset the index gives.
//!
//! The data of each entry is at its offset, after its `FPakEntry` again (of version 1 to 9's
//! form). Entries stored as they are and those of zlib or gzip are files (the latter of their
//! compressed blocks, which aqr-compress finds in them); those of other methods (Oodle) are
//! files of compressed contents ([`aqr_disk::Compressed::Opaque`]); encrypted and deleted ones
//! are the rest. An encrypted index is not read.

use super::ue::R;
use super::{file_with, u32le, u64le, within};
use aqr_disk::{Compressed, File};

const MAGIC: [u8; 4] = [0xE1, 0x12, 0x6F, 0x5A];
const ENCRYPTED: u8 = 1;
const DELETED: u8 = 2;

/// The footer of pak `d`: where its magic is, the version, the index's offset and size, the
/// names of the compression methods (version 8 on; empty before).
#[derive(Debug, Clone)]
pub struct Footer {
    pub at: usize,
    pub version: u32,
    pub index: std::ops::Range<usize>,
    pub methods: Vec<Vec<u8>>,
    pub encrypted: bool,
}

/// Whether `d` ends as a pak does ([`footer`]).
pub fn starts(d: &[u8]) -> bool {
    footer(d).is_some()
}

/// The footer of `d`, if it ends as a pak does.
pub fn footer(d: &[u8]) -> Option<Footer> {
    // (the bytes after the magic: 40 before version 8, then 4 or 5 names, and version 9's byte)
    for tail in [44, 204, 205, 172, 173] {
        let Some(at) = d.len().checked_sub(tail) else { continue };
        if d[at..at + 4] != MAGIC {
            continue;
        }
        let version = u32le(d, at + 4)?;
        let names = match (tail, version) {
            (44, 1..=7) => 0,
            (204 | 205, 8..) => 5,
            (172 | 173, 8..) => 4,
            _ => continue,
        };
        if (tail % 2 == 1) != (version == 9) || version > 11 {
            continue;
        }
        let Some(index) = within(d, u64le(d, at + 8)?, u64le(d, at + 16)?) else { continue };
        if index.end > at {
            continue;
        }
        let methods = (0..names).map(|k| d[at + 44 + 32 * k..][..32].split(|&b| b == 0).next().unwrap().to_vec());
        let encrypted = version >= 4 && at > 0 && d[at - 1] != 0;
        return Some(Footer { at, version, index, methods: methods.collect(), encrypted });
    }
    None
}

/// An entry as far as its data goes: the offset of its copy, its size compressed and not, the
/// method, the blocks, the flags.
#[derive(Debug, Clone, Copy)]
struct Entry {
    offset: u64,
    size: u64,
    raw: u64,
    method: u32,
    blocks: u32,
    flags: u8,
}

impl Entry {
    /// Bytes of its `FPakEntry` of `version` (before version 10's encoding): the copy before its
    /// data.
    fn header(&self, version: u32) -> usize {
        let mut n = 24 + 4 + 20;
        if version <= 1 {
            n += 8;
        }
        if version >= 3 {
            if self.method != 0 {
                n += 4 + 16 * self.blocks as usize;
            }
            n += 5;
        }
        n
    }
}

/// An `FPakEntry` of `version`.
fn entry(r: &mut R, version: u32) -> Option<Entry> {
    let (offset, size, raw) = (r.u64()?, r.u64()?, r.u64()?);
    let method = r.u32()?;
    if version <= 1 {
        r.skip(8)?;
    }
    r.skip(20)?;
    let (mut blocks, mut flags) = (0, 0);
    if version >= 3 {
        if method != 0 {
            blocks = r.u32()?;
            r.skip((blocks as usize).checked_mul(16)?)?;
        }
        flags = r.u8()?;
        r.skip(4)?;
    }
    Some(Entry { offset, size, raw, method, blocks, flags })
}

/// An entry encoded in bits (version 10 on).
fn encoded(r: &mut R) -> Option<Entry> {
    let v = r.u32()?;
    if v & 0x3f == 0x3f {
        r.skip(4)?;
    }
    let mut wide = |bit: u32| -> Option<u64> { if v & (1 << bit) != 0 { r.u32().map(u64::from) } else { r.u64() } };
    let offset = wide(31)?;
    let raw = wide(30)?;
    let method = (v >> 23) & 0x3f;
    let size = if method != 0 { wide(29)? } else { raw };
    let encrypted = v & (1 << 22) != 0;
    let blocks = (v >> 6) & 0xffff;
    if blocks > 0 && (encrypted || blocks != 1) {
        r.skip(blocks as usize * 4)?;
    }
    Some(Entry { offset, size, raw, method, blocks, flags: if encrypted { ENCRYPTED } else { 0 } })
}

/// The names and entries of the index of `d` (of `f`).
fn index(d: &[u8], f: &Footer) -> Option<Vec<(Vec<u8>, Entry)>> {
    let ix = &d[f.index.clone()];
    let mut r = R { d: ix, at: 0 };
    let mount = r.string()?;
    let n = r.u32()? as usize;
    // (an entry takes 12 bytes of the index at least)
    if n > ix.len() / 12 {
        return None;
    }
    let mut out = Vec::with_capacity(n);
    if f.version < 10 {
        for _ in 0..n {
            let name = r.string()?;
            let e = entry(&mut r, f.version)?;
            out.push(([&mount[..], &name].concat(), e));
        }
        return Some(out);
    }
    r.skip(8)?;
    let mut directory = None;
    for k in 0..2 {
        if r.u32()? != 0 {
            let (at, size) = (r.u64()?, r.u64()?);
            r.skip(20)?;
            if k == 1 {
                directory = within(d, at, size);
            }
        }
    }
    let size = r.u32()? as usize;
    let codes = r.at..r.at.checked_add(size)?;
    r.skip(size)?;
    let plain = r.u32()? as usize;
    let mut plains = Vec::with_capacity(plain.min(n));
    for _ in 0..plain {
        plains.push(entry(&mut r, f.version)?);
    }
    // the names, where the full directory index is: each directory's files, each with where
    // its entry is (of the encoded ones: its offset among them; else -1 - its index of the rest)
    let Some(dir) = directory else { return Some(Vec::new()) };
    let mut r = R { d: &d[dir], at: 0 };
    let dirs = r.u32()? as usize;
    for _ in 0..dirs {
        let path = r.string()?;
        let files = r.u32()? as usize;
        for _ in 0..files {
            let name = r.string()?;
            let at = r.u32()? as i32;
            let e = match at {
                0.. => encoded(&mut R { d: &ix[codes.clone()], at: at as usize })?,
                _ => *plains.get((-1 - at as i64) as usize)?,
            };
            out.push(([&mount[..], &path, &name].concat(), e));
        }
    }
    Some(out)
}

/// The files of pak `d`; None where it is no pak, or its index is encrypted or not read.
pub fn files(d: &[u8]) -> Option<Vec<File>> {
    let f = footer(d)?;
    if f.encrypted {
        return None;
    }
    let entries = index(d, &f)?;
    let mut files = Vec::with_capacity(entries.len());
    for (name, e) in entries {
        if e.flags & (ENCRYPTED | DELETED) != 0 || e.size == 0 {
            continue;
        }
        let h = e.header(f.version);
        let Some(extent) = within(d, e.offset.saturating_add(h as u64), e.size) else { continue };
        // (the copy of the entry before the data must tell the same sizes)
        if u64le(d, e.offset as usize + 8) != Some(e.size) || u64le(d, e.offset as usize + 16) != Some(e.raw) {
            continue;
        }
        let zlib = match f.version {
            ..8 => matches!(e.method, 1 | 2),
            _ => {
                e.method > 0
                    && f.methods
                        .get(e.method as usize - 1)
                        .is_some_and(|m| m.eq_ignore_ascii_case(b"zlib") || m.eq_ignore_ascii_case(b"gzip"))
            }
        };
        let compressed = (e.method != 0 && !zlib).then_some(Compressed::Opaque { len: e.raw });
        files.push(file_with(name, extent, compressed));
    }
    Some(files)
}
