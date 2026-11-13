//! Half-Life's WAD3 (the textures of GoldSrc): the layout of Quake's WAD2 ([`super::quake_wad`])
//! with the signature `WAD3`, the types differing (0x43 a texture, 0x40 a palette, 0x42 a
//! picture, 0x45 a font). Files and rest as there. (Written from the public description;
//! unverified on real files.)

use super::quake_wad::{entries, starts_with};
use aqr_disk::File;

pub fn starts(d: &[u8]) -> bool {
    starts_with(d, b"WAD3")
}

pub fn files(d: &[u8]) -> Option<Vec<File>> {
    if !starts(d) {
        return None;
    }
    entries(d, &|t| match t {
        0x43 => "mip",
        0x40 => "pal",
        _ => "lmp",
    })
}
