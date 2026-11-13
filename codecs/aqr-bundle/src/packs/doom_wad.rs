//! id Software's WAD (Doom, Heretic, Hexen, and their source ports): `IWAD` or `PWAD`, the count
//! of lumps and the directory's offset (i32 each, little endian). The directory is entries of 16
//! bytes: the lump's offset and size (i32 each) and its name (8 bytes, NUL-padded).
//!
//! Each lump of size over 0 whose data lies within the file is a file `NNNN_NAME.EXT` (NNNN
//! its index, as names repeat: `THINGS`, `SIDEDEFS` of each map; unprintable bytes and path
//! separators of the name are `_`), its extension by its magic where it has one (`png`, `mid`,
//! `mus`, `ogg`, `wav`, `mp3`, `flac`), else `lmp`. Markers (size 0) are no files; the rest is
//! the header and the directory. (Written from the public description; unverified on real
//! files.)

use super::{file, u32le, within};
use aqr_disk::File;

pub fn starts(d: &[u8]) -> bool {
    (d.starts_with(b"IWAD") || d.starts_with(b"PWAD"))
        && u32le(d, 4).is_some_and(|n| (n as u64) * 16 <= d.len() as u64)
        && u32le(d, 8).is_some_and(|o| o >= 12)
}

pub fn files(d: &[u8]) -> Option<Vec<File>> {
    if !starts(d) {
        return None;
    }
    let dir = within(d, u32le(d, 8)? as u64, u32le(d, 4)? as u64 * 16)?;
    let mut files = Vec::new();
    for (i, e) in d[dir].as_chunks::<16>().0.iter().enumerate() {
        let (offset, size) = (u32le(e, 0)?, u32le(e, 4)?);
        let Some(body) = within(d, offset as u64, size as u64).filter(|b| !b.is_empty()) else { continue };
        let ext = match &d[body.clone()] {
            [0x89, b'P', b'N', b'G', ..] => "png",
            [b'M', b'T', b'h', b'd', ..] => "mid",
            [b'M', b'U', b'S', 0x1a, ..] => "mus",
            [b'O', b'g', b'g', b'S', ..] => "ogg",
            [b'R', b'I', b'F', b'F', ..] => "wav",
            [b'f', b'L', b'a', b'C', ..] => "flac",
            [b'I', b'D', b'3', ..] | [0xff, 0xfb | 0xfa | 0xf3 | 0xf2, ..] => "mp3",
            _ => "lmp",
        };
        let name = e[8..16].iter().take_while(|&&c| c != 0).map(|&c| label(c)).collect::<String>();
        files.push(file(format!("{i:04}_{name}.{ext}").into_bytes(), body));
    }
    Some(files)
}

/// A byte of a lump's name as a character of a file name.
pub fn label(c: u8) -> char {
    if c.is_ascii_graphic() && !b"/\\:*?\"<>|".contains(&c) { c as char } else { '_' }
}
