//! Electronic Arts' BIG archives (`BIGF`, `BIG4`: the Command & Conquer, Battlefield and other
//! games). Written from public descriptions and unverified on real files: no sample was at
//! hand, only archives the tests write.
//!
//! The header: the magic, the archive's size (u32, little endian), the count of entries (u32)
//! and the size of the header (u32), the last two big endian. Each entry: the offset (u32), the
//! size (u32; both big endian) and the NUL-terminated name (`\` made `/`), in order from byte 16.
//!
//! An entry is a plain file, but where it starts with RefPack's signature (`10 FB`, or `90 FB`
//! with 4-byte sizes: flags `0x10`/`0x90`, then `FB`), which is a file of compressed contents
//! ([`aqr_disk::Compressed::Opaque`]) with the original size that its header tells (3 or 4
//! bytes, big endian). A header with the flag `0x01` (a compressed size comes first) is not
//! read, its order not being pinned down: it is a plain file. Entries out of the archive are
//! left out.

use super::{cstr, file, u32be, within};
use aqr_disk::{Compressed, File};

pub fn starts(d: &[u8]) -> bool {
    (d.starts_with(b"BIGF") || d.starts_with(b"BIG4")) && d.len() >= 16
}

pub fn files(d: &[u8]) -> Option<Vec<File>> {
    if !starts(d) {
        return None;
    }
    let count = u32be(d, 8)? as usize;
    let mut at = 16;
    let mut out = Vec::new();
    for _ in 0..count {
        let (offset, size) = (u32be(d, at)?, u32be(d, at + 4)?);
        let (name, next) = cstr(d, at + 8)?;
        at = next;
        let Some(r) = within(d, offset as u64, size as u64) else { continue };
        if r.is_empty() {
            continue;
        }
        let mut name = name.to_vec();
        for b in &mut name {
            if *b == b'\\' {
                *b = b'/';
            }
        }
        let mut f = file(name, r.clone());
        let h = &d[r.start..r.end];
        if h.len() >= 5 && h[0] & 0x7e == 0x10 && h[1] == 0xfb && h[0] & 1 == 0 {
            let n = if h[0] & 0x80 != 0 { 4 } else { 3 };
            if let Some(s) = h.get(2..2 + n) {
                let len = s.iter().fold(0u64, |a, &b| a << 8 | b as u64);
                f.compressed = Some(Compressed::Opaque { len });
            }
        }
        out.push(f);
    }
    Some(out)
}
