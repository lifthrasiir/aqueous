//! Valve's VPK (the Source engine's packs), versions 1 and 2: the directory file
//! (`*_dir.vpk`, or a VPK of one file), little endian.
//!
//! The header: the signature `34 12 AA 55`, the version (u32) and the tree's size (u32), then in
//! version 2 the sizes of the file data, of the archives' MD5s, of the other MD5s and of the
//! signature (u32 each). The tree follows: for each extension (a string of a NUL; an empty one
//! ends the tree), each directory (an empty one ends the extension's), each file name (an empty
//! one ends the directory's): the file's CRC-32 (u32), its preload bytes' count (u16), its
//! archive's index (u16), its offset and size in that archive (u32 each), `FF FF`, then the
//! preload bytes. The archive `7FFF` is the directory file itself, its data after the tree.
//!
//! A file in the directory file is its preload bytes and its data there ([`files`]); a file in
//! another archive (`*_000.vpk`, …, of no header) is its data there, the archive taken apart
//! by the directory file's tree ([`archive_files`]), its preload bytes the directory file's rest.

use std::ops::Range;

use super::{cstr, file, u16le, u32le, within};
use aqr_disk::File;

const MAGIC: [u8; 4] = [0x34, 0x12, 0xAA, 0x55];
/// The archive's index of the data in the directory file.
pub const HERE: u16 = 0x7fff;

pub fn starts(d: &[u8]) -> bool {
    d.starts_with(&MAGIC) && u32le(d, 4).is_some_and(|v| v == 1 || v == 2)
}

/// A file of the tree: its path, its preload bytes (in the directory file), its archive's index,
/// and its offset and size there.
#[derive(Debug, Clone)]
pub struct Entry {
    pub path: Vec<u8>,
    pub preload: Range<usize>,
    pub archive: u16,
    pub offset: u32,
    pub size: u32,
}

/// The tree of VPK `d`, and where the data in it starts; None where it is no VPK, or its tree
/// does not read.
pub fn tree(d: &[u8]) -> Option<(Vec<Entry>, usize)> {
    if !starts(d) {
        return None;
    }
    let header = if u32le(d, 4)? == 1 { 12 } else { 28 };
    let tree = within(d, header as u64, u32le(d, 8)? as u64)?;
    let mut out = Vec::new();
    let mut at = tree.start;
    loop {
        let (ext, next) = cstr(d, at)?;
        at = next;
        if ext.is_empty() {
            break;
        }
        loop {
            let (dir, next) = cstr(d, at)?;
            at = next;
            if dir.is_empty() {
                break;
            }
            loop {
                let (name, next) = cstr(d, at)?;
                at = next;
                if name.is_empty() {
                    break;
                }
                let (preload, archive) = (u16le(d, at + 4)? as usize, u16le(d, at + 6)?);
                let (offset, size) = (u32le(d, at + 8)?, u32le(d, at + 12)?);
                if u16le(d, at + 16)? != 0xffff {
                    return None;
                }
                let preload = at + 18..at + 18 + preload;
                at = preload.end;
                if at > tree.end {
                    return None;
                }
                let mut path = Vec::with_capacity(dir.len() + name.len() + ext.len() + 2);
                if dir != b" " {
                    path.extend_from_slice(dir);
                    path.push(b'/');
                }
                path.extend_from_slice(name);
                if ext != b" " {
                    path.push(b'.');
                    path.extend_from_slice(ext);
                }
                out.push(Entry { path, preload, archive, offset, size });
            }
        }
    }
    Some((out, tree.end))
}

/// The files of VPK `d` whose data is in it (their preload bytes, then their data); None where
/// it is no VPK, or its tree does not read.
pub fn files(d: &[u8]) -> Option<Vec<File>> {
    let (entries, data) = tree(d)?;
    let mut files = Vec::new();
    for e in entries.into_iter().filter(|e| e.archive == HERE) {
        let Some(body) = within(d, data as u64 + e.offset as u64, e.size as u64) else { continue };
        let extents = [e.preload, body].into_iter().filter(|r| !r.is_empty()).collect();
        files.push(File { name: e.path, extents, rank: 0, compressed: None });
    }
    Some(files)
}

/// The files of the archive of index `archive` of the tree `entries`, of `len` bytes: those
/// whose data it holds (without their preload bytes, which are the directory file's).
pub fn archive_files(entries: &[Entry], archive: u16, len: usize) -> Vec<File> {
    let of = entries.iter().filter(|e| e.archive == archive && e.size > 0);
    let fits = of.filter(|e| e.offset as usize + e.size as usize <= len);
    fits.map(|e| file(e.path.clone(), e.offset as usize..e.offset as usize + e.size as usize)).collect()
}

/// The bytes an archive of index `archive` of the tree `entries` holds at least: the end of
/// the last of its files' data; None where none is in it.
pub fn archive_len(entries: &[Entry], archive: u16) -> Option<usize> {
    entries.iter().filter(|e| e.archive == archive).map(|e| e.offset as usize + e.size as usize).max()
}
