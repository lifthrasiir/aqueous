//! CRI's AFS2 archives (`.awb`, the sound banks of CRI ADX2 / CRIWARE), little endian.
//!
//! The header: `AFS2`, a version byte, the size of an offset (byte 5: 2 or 4), the size of an
//! id (byte 6: 2 or 4), a zero byte, the file count (u32), the alignment (u16) and a subkey
//! (u16, of the encryption of the audio; unused here). The ids follow (the id size each), then
//! `count + 1` offsets (the offset size each): the start of each file, and the end of the last.
//! A file starts at its offset aligned up to the alignment and ends at the next offset (not
//! aligned).
//!
//! Each file is taken as `<id>.<ext>`, the extension by its magic: `.hca` (`HCA\0`, each byte
//! possibly with its high bit set by the HCA's masking), `.adx` (`80 00`), `.wav` (`RIFF`), `.ogg` (`OggS`), else `.bin`. Other
//! `.awb` files (`QUAVMAGI`, ...) are not AFS2: not taken. The header and the padding between
//! files are the rest.

use std::ops::Range;

use super::{u16le, u32le, within};
use aqr_disk::File;

pub fn starts(d: &[u8]) -> bool {
    d.starts_with(b"AFS2") && matches!(d.get(5), Some(2 | 4)) && matches!(d.get(6), Some(2 | 4))
}

/// The extension of a member by its first bytes.
pub fn ext(d: &[u8]) -> &'static str {
    let m = |b: &[u8]| d.len() >= b.len() && d.iter().zip(b).all(|(x, y)| x & 0x7f == *y);
    if m(b"HCA\0") {
        "hca"
    } else if d.starts_with(&[0x80, 0x00]) {
        "adx"
    } else if d.starts_with(b"RIFF") {
        "wav"
    } else if d.starts_with(b"OggS") {
        "ogg"
    } else {
        "bin"
    }
}

/// The members of AFS2 `d`: id and the range of each, in the archive's order; None where it is
/// none or its tables do not fit.
pub fn members(d: &[u8]) -> Option<Vec<(u32, Range<usize>)>> {
    if !starts(d) {
        return None;
    }
    let (osz, isz) = (d[5] as usize, d[6] as usize);
    let count = u32le(d, 8)? as usize;
    let align = (u16le(d, 12)? as usize).max(1);
    let ids = 16usize;
    let offs = ids.checked_add(count.checked_mul(isz)?)?;
    let end = offs.checked_add(count.checked_add(1)?.checked_mul(osz)?)?;
    if end > d.len() {
        return None;
    }
    let num = |at: usize, n: usize| -> u64 { d[at..at + n].iter().rev().fold(0u64, |a, &b| a << 8 | b as u64) };
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let id = num(ids + i * isz, isz) as u32;
        let (a, b) = (num(offs + i * osz, osz), num(offs + (i + 1) * osz, osz));
        let a = a.div_ceil(align as u64) * align as u64;
        if b > a
            && let Some(r) = within(d, a, b - a)
        {
            out.push((id, r));
        }
    }
    Some(out)
}

pub fn files(d: &[u8]) -> Option<Vec<File>> {
    let m = members(d)?;
    let out: Vec<File> =
        m.into_iter().map(|(id, r)| super::file(format!("{id}.{}", ext(&d[r.clone()])).into_bytes(), r)).collect();
    (!out.is_empty()).then_some(out)
}
