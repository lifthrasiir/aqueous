//! RAR5 archives as aqr-rar reads them (RARLAB's technote, `docs/rar5-decoding.md` §1): the
//! headers in turn, each file's and service header's data area, the solid streams.

use std::ops::Range;

use aqr_common::crc32;

pub const SIGNATURE: &[u8; 8] = b"Rar!\x1a\x07\x01\x00";

/// A file or service header's data.
#[derive(Clone, Debug)]
pub struct Entry {
    /// A service header (named data: a comment, quick open, a recovery record, …).
    pub service: bool,
    pub name: Range<usize>,
    /// The data area.
    pub data: Range<usize>,
    /// The unpacked size, None where unknown.
    pub size: Option<u64>,
    pub crc: Option<u32>,
    /// The compression information: algorithm version (0 or 1), solid, method, dictionary.
    pub version: u8,
    pub solid: bool,
    pub method: u8,
    pub dict: u64,
    /// Continued from the volume before, continuing in the next.
    pub split: bool,
    pub encrypted: bool,
    pub dir: bool,
}

#[derive(Debug)]
pub struct Rar5 {
    pub entries: Vec<Entry>,
    /// The end of the archive's end header, or of the last header read.
    pub end: usize,
    pub volume: bool,
}

/// A vint at `*at` of `d`: 7 bits a byte, the lowest first, the top bit set where more follow.
pub fn vint(d: &[u8], at: &mut usize) -> Option<u64> {
    let mut v = 0u64;
    for i in 0..10 {
        let b = *d.get(*at)?;
        *at += 1;
        if i == 9 && b > 1 {
            return None;
        }
        v |= ((b & 0x7f) as u64) << (7 * i);
        if b & 0x80 == 0 {
            return Some(v);
        }
    }
    None
}

/// Whether `d` starts with a RAR5 archive.
pub fn starts(d: &[u8]) -> bool {
    d.starts_with(SIGNATURE)
}

/// The dictionary of compression information `ci`, None where it is of no known version.
pub fn dict(ci: u64) -> Option<(u8, u64)> {
    let v = (ci & 0x3f) as u8;
    let n = (ci >> 10) & 0x1f;
    let base = 128u64 << 10 << n;
    match v {
        0 if n <= 15 && ci >> 15 == 0 => Some((0, base)),
        1 if ci >> 21 == 0 => {
            let f = (ci >> 15) & 0x1f;
            // (the data of the version-0 algorithm in a field of version 1)
            let alg = if ci & 0x10_0000 != 0 { 0 } else { 1 };
            Some((alg, base + base / 32 * f))
        }
        _ => None,
    }
}

/// Reads the archive at the start of `d`. None where it is none, or its headers are encrypted
/// or damaged before any file.
pub fn parse(d: &[u8]) -> Option<Rar5> {
    if !starts(d) {
        return None;
    }
    let mut at = SIGNATURE.len();
    let mut entries = Vec::new();
    let mut volume = false;
    let mut end = at;
    loop {
        let start = at;
        let Some(crc) = d.get(at..at + 4) else { break };
        let crc = u32::from_le_bytes(crc.try_into().unwrap());
        let mut p = at + 4;
        let Some(size) = vint(d, &mut p) else { break };
        let Some(hend) = usize::try_from(size).ok().and_then(|s| p.checked_add(s)).filter(|&e| e <= d.len()) else {
            break;
        };
        if !(2..=2 << 20).contains(&size) || crc32::hash(&d[at + 4..hend]) != crc {
            break;
        }
        let h = &d[..hend];
        let Some(kind) = vint(h, &mut p) else { break };
        let Some(flags) = vint(h, &mut p) else { break };
        let extra = if flags & 1 != 0 { vint(h, &mut p) } else { Some(0) };
        let data = if flags & 2 != 0 { vint(h, &mut p) } else { Some(0) };
        let (Some(extra), Some(data)) = (extra, data) else { break };
        let Some(dend) = usize::try_from(data).ok().and_then(|n| hend.checked_add(n)).filter(|&e| e <= d.len()) else {
            break;
        };
        let Some(xstart) = usize::try_from(extra).ok().and_then(|x| hend.checked_sub(x)).filter(|&x| x >= p) else {
            break;
        };
        match kind {
            1 => {
                let Some(af) = vint(h, &mut p) else { break };
                volume = af & 1 != 0;
            }
            2 | 3 => {
                let Some(e) = file(h, p, xstart, hend, dend, flags, kind == 3) else { break };
                entries.push(e);
            }
            4 => {
                // headers encrypted from here on
                end = start;
                break;
            }
            5 => {
                end = dend;
                break;
            }
            _ if flags & 4 != 0 => {}
            _ => break,
        }
        at = dend;
        end = at;
    }
    if entries.is_empty() {
        return None;
    }
    Some(Rar5 { entries, end, volume })
}

/// A file or service header: its fields from `p`, its extra area at `x..hend`, its data to `dend`.
fn file(h: &[u8], mut p: usize, x: usize, hend: usize, dend: usize, flags: u64, service: bool) -> Option<Entry> {
    let ff = vint(h, &mut p)?;
    let size = vint(h, &mut p)?;
    let _attr = vint(h, &mut p)?;
    if ff & 2 != 0 {
        p += 4;
    }
    let crc = if ff & 4 != 0 {
        let c = u32::from_le_bytes(h.get(p..p + 4)?.try_into().unwrap());
        p += 4;
        Some(c)
    } else {
        None
    };
    let ci = vint(h, &mut p)?;
    let _os = vint(h, &mut p)?;
    let nl = usize::try_from(vint(h, &mut p)?).ok()?;
    let name = p..p.checked_add(nl)?;
    if name.end > x {
        return None;
    }
    // the extra records: an encryption record (type 1)
    let mut encrypted = false;
    let mut q = x;
    while q < hend {
        let n = usize::try_from(vint(h, &mut q)?).ok()?;
        let rend = q.checked_add(n).filter(|&e| e <= hend)?;
        if vint(h, &mut q)? == 1 {
            encrypted = true;
        }
        q = rend;
    }
    let (version, dict) = match ci & 0x380 {
        0 => (0, dict(ci).map_or(0, |d| d.1)),
        _ => dict(ci).unwrap_or((u8::MAX, 0)),
    };
    Some(Entry {
        service,
        name,
        data: hend..dend,
        size: (ff & 8 == 0).then_some(size),
        crc,
        version,
        solid: ci & 0x40 != 0,
        method: ((ci >> 7) & 7) as u8,
        dict,
        split: flags & 0x18 != 0,
        encrypted,
        dir: ff & 1 != 0,
    })
}
