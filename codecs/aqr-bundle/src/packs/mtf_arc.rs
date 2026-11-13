//! Capcom's MT Framework archives (`ARC\0`; Monster Hunter, Resident Evil 5, Devil May Cry 4
//! and others), little endian. Written from public descriptions and unverified on real files: no
//! sample was at hand, only archives the tests write.
//!
//! The header: the magic, the version (u16) and the count of entries (u16). Each entry, 80 bytes
//! from byte 8: the name (64 bytes, NUL-padded, without an extension; `\` made `/`), the type's
//! hash (u32), the size packed (u32), the size unpacked (u32, whose top four bits are flags) and
//! the offset (u32). An entry whose sizes differ is a zlib stream, else it is stored; either way
//! it is a plain file of the packed size's bytes at the offset.
//!
//! The extension tells the type in the game's own hash; the table of the hashes is not pinned
//! down, so a file is named `NAME.HASH` with the hash in 8 hex digits.

use super::{file, slashes, u16le, u32le, within};
use aqr_disk::File;

pub fn starts(d: &[u8]) -> bool {
    d.starts_with(b"ARC\0")
        && u16le(d, 6).is_some_and(|n| 8 + n as usize * 80 <= d.len())
        && u16le(d, 4).is_some_and(|v| v < 0x100)
}

pub fn files(d: &[u8]) -> Option<Vec<File>> {
    if !starts(d) {
        return None;
    }
    let count = u16le(d, 6)? as usize;
    let mut out = Vec::new();
    for i in 0..count {
        let e = d.get(8 + i * 80..8 + i * 80 + 80)?;
        let n = e[..64].iter().position(|&b| b == 0).unwrap_or(64);
        let mut name = e[..n].to_vec();
        slashes(&mut name);
        name.extend_from_slice(format!(".{:08x}", u32le(e, 64)?).as_bytes());
        // Stored or zlib, the bytes in the archive are the packed size's.
        let (packed, offset) = (u32le(e, 68)?, u32le(e, 76)?);
        let Some(r) = within(d, offset as u64, packed as u64) else { continue };
        if !r.is_empty() {
            out.push(file(name, r));
        }
    }
    Some(out)
}
