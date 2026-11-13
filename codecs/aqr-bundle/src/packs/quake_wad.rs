//! Quake's WAD2 (textures, pictures, palettes of the Quake engine), little endian: the signature
//! `WAD2`, the count of entries and the directory's offset (i32 each). The directory is entries
//! of 32 bytes: the offset, the size on disk and the size (i32 each), the type (u8), the
//! compression (u8, 0 none), 2 bytes of padding and the name (16 bytes, NUL-padded). Half-Life's
//! WAD3 has the same layout ([`super::hl_wad`]).
//!
//! Each entry of size on disk over 0 whose data lies within the file is a file `NNNN_NAME.EXT`
//! (NNNN its index; the extension `mip` for textures, `pal` for palettes, else `lmp`); one with a
//! compression is a file of compressed contents ([`aqr_disk::Compressed::Opaque`]) of its size.
//! The rest is the header and the directory. (Written from the public description; unverified on
//! real files.)

use super::doom_wad::label;
use super::{file_with, u32le, within};
use aqr_disk::{Compressed, File};

pub fn starts(d: &[u8]) -> bool {
    starts_with(d, b"WAD2")
}

pub(super) fn starts_with(d: &[u8], magic: &[u8; 4]) -> bool {
    d.starts_with(magic)
        && u32le(d, 4).is_some_and(|n| (n as u64) * 32 <= d.len() as u64)
        && u32le(d, 8).is_some_and(|o| o >= 12)
}

pub fn files(d: &[u8]) -> Option<Vec<File>> {
    if !starts(d) {
        return None;
    }
    entries(d, &|t| match t {
        0x44 => "mip",
        0x40 => "pal",
        _ => "lmp",
    })
}

/// The files of WAD2/3 `d` (of a magic checked), the extension by the type.
pub(super) fn entries(d: &[u8], ext: &dyn Fn(u8) -> &'static str) -> Option<Vec<File>> {
    let dir = within(d, u32le(d, 8)? as u64, u32le(d, 4)? as u64 * 32)?;
    let mut files = Vec::new();
    for (i, e) in d[dir].as_chunks::<32>().0.iter().enumerate() {
        let (offset, disk, size) = (u32le(e, 0)?, u32le(e, 4)?, u32le(e, 8)?);
        let Some(body) = within(d, offset as u64, disk as u64).filter(|b| !b.is_empty()) else { continue };
        let name = e[16..32].iter().take_while(|&&c| c != 0).map(|&c| label(c)).collect::<String>();
        let compressed = (e[13] != 0).then_some(Compressed::Opaque { len: size as u64 });
        let name = format!("{i:04}_{name}.{}", ext(e[12])).into_bytes();
        files.push(file_with(name, body, compressed));
    }
    Some(files)
}
