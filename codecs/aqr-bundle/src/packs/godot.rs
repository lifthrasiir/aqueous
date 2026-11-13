//! Godot's packs (`.pck`, `GDPC`): version 1 (Godot 3), 2 (4.0 to 4.3), 3 and 4 (4.4 on).
//!
//! The header: `GDPC`, the version and the engine's (u32 each), then from version 2 the pack's
//! flags (u32: 1 the directory encrypted, 2 the files' base relative to the pack's start) and the
//! files' base (u64), from version 3 the directory's offset (u64), and 16 reserved u32. The
//! directory follows (versions 1 and 2) or is at its offset: the count of files (u32), then
//! each file: the path's length (u32, its NULs to a multiple of 4 included) and bytes, its
//! offset from the base and size (u64), its MD5 (16 bytes), and from version 2 its flags (u32: 1
//! encrypted). Little endian throughout.

use super::{file, u32le, u64le, within};
use aqr_disk::File;

const MAGIC: &[u8; 4] = b"GDPC";
/// Pack flags: the directory encrypted.
const DIR_ENCRYPTED: u32 = 1;
/// File flags: the file encrypted.
const FILE_ENCRYPTED: u32 = 1;

pub fn starts(d: &[u8]) -> bool {
    d.starts_with(MAGIC) && u32le(d, 4).is_some_and(|v| (1..=4).contains(&v))
}

/// The files of pack `d`, but the encrypted ones; None where it is no pack, or its directory
/// is encrypted or does not fit.
pub fn files(d: &[u8]) -> Option<Vec<File>> {
    if !starts(d) {
        return None;
    }
    let version = u32le(d, 4)?;
    let (mut at, mut base) = (20, 0);
    if version >= 2 {
        if u32le(d, 20)? & DIR_ENCRYPTED != 0 {
            return None;
        }
        base = u64le(d, 24)?;
        at = 32;
    }
    if version >= 3 {
        at = usize::try_from(u64le(d, 32)?).ok()?;
    } else {
        at += 64;
    }
    let n = u32le(d, at)? as usize;
    at += 4;
    // (each file takes 36 bytes of the directory at least)
    if n > d.len().saturating_sub(at) / 36 {
        return None;
    }
    let mut files = Vec::with_capacity(n);
    for _ in 0..n {
        let len = u32le(d, at)? as usize;
        let path = d.get(at + 4..(at + 4).checked_add(len)?)?;
        at += 4 + len;
        let (offset, size) = (u64le(d, at)?, u64le(d, at + 8)?);
        at += 32;
        let flags = match version {
            1 => 0,
            _ => {
                at += 4;
                u32le(d, at - 4)?
            }
        };
        let name = path.iter().rposition(|&b| b != 0).map_or(&[][..], |k| &path[..=k]);
        let name = name.strip_prefix(b"res://").unwrap_or(name);
        if flags & FILE_ENCRYPTED == 0
            && let Some(extent) = within(d, base.checked_add(offset)?, size)
        {
            files.push(file(name.to_vec(), extent));
        }
    }
    Some(files)
}
