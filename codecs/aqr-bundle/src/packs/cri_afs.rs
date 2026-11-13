//! CRI's old AFS archives (`.afs`, CRI ADX / sound banks of the Dreamcast and PS2 era),
//! little endian. UNVERIFIED on real files: written from public descriptions of the format and
//! tested only on packs the tests write.
//!
//! The header: `AFS\0`, the file count (u32), then `(offset, size)` (u32 each) for each file.
//! After them, at `8 + 8 * count`, an optional pair (offset, size) of a name table: 48 bytes
//! for each file, a 32-byte NUL-padded name, six u16 of the date and a u32 of the size. The
//! table is taken where it fits and is `48 * count` long (it usually follows the last file).
//! Files are named by it; without it `<index>.<ext>` (the extension by the magic of
//! [`super::cri_awb::ext`]). The header, the name table and the padding are the rest.

use super::{cri_awb, u32le, within};
use aqr_disk::File;

pub fn starts(d: &[u8]) -> bool {
    d.starts_with(b"AFS\0")
}

pub fn files(d: &[u8]) -> Option<Vec<File>> {
    if !starts(d) {
        return None;
    }
    let count = u32le(d, 4)? as usize;
    let table = count.checked_mul(8)?.checked_add(8)?;
    if table > d.len() {
        return None;
    }
    let names = (|| {
        let (off, len) = (u32le(d, table)? as u64, u32le(d, table + 4)? as u64);
        (len == 48 * count as u64).then(|| within(d, off, len)).flatten()
    })();
    let mut out = Vec::new();
    for i in 0..count {
        let (off, size) = (u32le(d, 8 + 8 * i)? as u64, u32le(d, 12 + 8 * i)? as u64);
        let Some(r) = within(d, off, size).filter(|r| !r.is_empty()) else { continue };
        let name = names
            .as_ref()
            .map(|n| &d[n.start + 48 * i..n.start + 48 * i + 32])
            .map(|s| &s[..s.iter().position(|&b| b == 0).unwrap_or(32)])
            .filter(|s| !s.is_empty())
            .map(|s| s.to_vec())
            .unwrap_or_else(|| format!("{i:05}.{}", cri_awb::ext(&d[r.clone()])).into_bytes());
        out.push(super::file(name, r));
    }
    (!out.is_empty()).then_some(out)
}
