//! Bethesda's BSA archives (`BSA\0`), versions 103 (Oblivion), 104 (Fallout 3, New Vegas,
//! Skyrim) and 105 (Skyrim Special Edition), and Morrowind's (no magic: the u32 `0x100`), little
//! endian. Written from the public descriptions (UESP wiki) and unverified on real files: no
//! sample was at hand, only archives the tests write.
//!
//! The header (36 bytes): the magic, the version, the offset of the folder records (36), the
//! archive flags, the counts of folders and of files, the total lengths of the folder names and
//! of the file names, the file flags (u16) and a padding (u16). The flags: 1 folder names are
//! included, 2 file names are, 4 files are compressed by default, 0x40 Xbox 360 (left out),
//! 0x100 the files' names are embedded (a bstring before each file's data; only from version
//! 104).
//!
//! The folder records (hash u64, file count u32, then the offset u32 -- in 105 a padding u32 and
//! the offset u64) are followed by the file records' blocks, one for each folder in order: the
//! folder's name (a bzstring: a length byte, the name and its NUL; if the names are included),
//! then its files' records (hash u64, size u32, offset u32). The folder record's offset less the
//! total length of the file names is where its block is. The file names follow all of the
//! blocks, NUL-terminated, in the order of the records. A file's name is its folder's, a
//! backslash (made `/`) and its own; where the names are not included, the hashes in hex.
//!
//! A file's size has the bit 30 toggling the archive's default of compression, and the data at
//! its offset: the embedded name if any (a length byte and the name), and where compressed the
//! original size (u32) and the stream. A file stored is a plain file. Compressed: in 103 and 104
//! a zlib stream, a plain file of its bytes; in 105 an LZ4 frame, a file of compressed contents
//! ([`aqr_disk::Compressed::Opaque`]) of its bytes with the original size.
//!
//! Morrowind's: the hash table's offset (from the end of the 12-byte header) and the file count;
//! the records of (size, offset) for each file, the offsets of the names (from the names' start),
//! the NUL-terminated names, the hash table (8 bytes each), then the data, to which the records'
//! offsets are relative. The files are plain.
//!
//! Left out: Xbox 360 archives, files out of the archive. Whether the size of a file with an
//! embedded name counts that name is not pinned down; it is taken to.

use super::{cstr, file, u32le, u64le, within};
use aqr_disk::{Compressed, File};

fn morrowind(d: &[u8]) -> bool {
    let (Some(hashes), Some(count)) = (u32le(d, 4), u32le(d, 8)) else { return false };
    d.starts_with(&[0, 1, 0, 0])
        && count > 0
        && hashes as u64 >= count as u64 * 12
        && 12 + hashes as u64 + count as u64 * 8 <= d.len() as u64
}

pub fn starts(d: &[u8]) -> bool {
    (d.starts_with(b"BSA\0") && matches!(u32le(d, 4), Some(103..=105))) || morrowind(d)
}

pub fn files(d: &[u8]) -> Option<Vec<File>> {
    if d.starts_with(b"BSA\0") {
        bsa(d)
    } else if morrowind(d) {
        old(d)
    } else {
        None
    }
}

fn path(folder: &[u8], name: &[u8]) -> Vec<u8> {
    let mut p = Vec::with_capacity(folder.len() + name.len() + 1);
    if !folder.is_empty() {
        p.extend_from_slice(folder);
        p.push(b'\\');
    }
    p.extend_from_slice(name);
    for b in &mut p {
        if *b == b'\\' {
            *b = b'/';
        }
    }
    p
}

fn old(d: &[u8]) -> Option<Vec<File>> {
    let (hashes, count) = (u32le(d, 4)? as u64, u32le(d, 8)? as u64);
    let data = 12 + hashes + count * 8;
    let names = 12 + count * 12;
    let mut out = Vec::new();
    for i in 0..count {
        let (size, offset) = (u32le(d, (12 + i * 8) as usize)?, u32le(d, (12 + i * 8 + 4) as usize)?);
        let at = u32le(d, (12 + count * 8 + i * 4) as usize)? as u64;
        let (name, _) = cstr(d, usize::try_from(names + at).ok()?)?;
        let Some(r) = within(d, data + offset as u64, size as u64) else { continue };
        if !r.is_empty() {
            out.push(file(path(b"", name), r));
        }
    }
    Some(out)
}

fn bsa(d: &[u8]) -> Option<Vec<File>> {
    let version = u32le(d, 4)?;
    if !matches!(version, 103..=105) {
        return None;
    }
    let (folders, flags) = (u32le(d, 8)? as usize, u32le(d, 12)?);
    let (nfolders, names_len) = (u32le(d, 16)? as usize, u32le(d, 28)? as u64);
    if flags & 0x40 != 0 {
        return None;
    }
    let (named_dirs, named_files) = (flags & 1 != 0, flags & 2 != 0);
    let by_default = flags & 4 != 0;
    let embedded = version >= 104 && flags & 0x100 != 0;
    let size = if version == 105 { 24 } else { 16 };
    within(d, folders as u64, nfolders as u64 * size as u64)?;

    // Each folder's name (or its hash) and its files' (hash, size, offset).
    let mut blocks = Vec::new();
    let mut end = 0;
    for i in 0..nfolders {
        let at = folders + i * size;
        let (hash, count) = (u64le(d, at)?, u32le(d, at + 8)? as usize);
        let offset = if version == 105 { u64le(d, at + 16)? } else { u32le(d, at + 12)? as u64 };
        let mut at = usize::try_from(offset.checked_sub(names_len)?).ok()?;
        let name = if named_dirs {
            let len = *d.get(at)? as usize;
            let s = d.get(at + 1..at + 1 + len)?;
            at += 1 + len;
            s.strip_suffix(&[0]).unwrap_or(s).to_vec()
        } else {
            format!("{hash:016x}").into_bytes()
        };
        within(d, at as u64, count as u64 * 16)?;
        let mut records = Vec::with_capacity(count);
        for k in 0..count {
            let r = at + k * 16;
            records.push((u64le(d, r)?, u32le(d, r + 8)?, u32le(d, r + 12)?));
        }
        end = at + count * 16;
        blocks.push((name, records));
    }

    let mut out = Vec::new();
    let mut names = end;
    for (folder, records) in blocks {
        for (hash, size, offset) in records {
            let name = if named_files {
                let (s, next) = cstr(d, names)?;
                names = next;
                s.to_vec()
            } else {
                format!("{hash:016x}").into_bytes()
            };
            let compressed = by_default != (size & 0x4000_0000 != 0);
            let Some(r) = within(d, offset as u64, (size & 0x3fff_ffff) as u64) else { continue };
            let (mut start, end) = (r.start, r.end);
            if embedded {
                start += 1 + *d.get(start)? as usize;
            }
            let mut packed = None;
            if compressed {
                let original = u32le(d, start)?;
                start += 4;
                if version == 105 {
                    packed = Some(Compressed::Opaque { len: original as u64 });
                }
            }
            if start >= end {
                continue;
            }
            let mut f = file(path(&folder, &name), start..end);
            f.compressed = packed;
            out.push(f);
        }
    }
    Some(out)
}
