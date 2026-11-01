//! LHA archives read: their headers one after another from the start (`FORMAT.md`, "The
//! archive"), each followed by its file's packed data.

use std::ops::Range;

/// An entry: its header and its data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Its header, the extended headers of levels 1 to 3 within.
    pub header: Range<usize>,
    pub level: u8,
    /// The method id (`-lh5-` and so on).
    pub method: [u8; 5],
    /// The file's name, where the header has one (the base header's, or the extended header's of
    /// type 1).
    pub name: Range<usize>,
    /// The original size, and the CRC-16 of the file.
    pub size: u64,
    pub crc: u16,
    /// The packed data.
    pub data: Range<usize>,
}

/// An archive read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lha {
    pub entries: Vec<Entry>,
    /// Where the headers end: after the end byte 0, at the end of the data, or at the first
    /// byte that is not a header.
    pub end: usize,
}

fn le(d: &[u8], at: usize, n: usize) -> Option<u64> {
    let s = d.get(at..at.checked_add(n)?)?;
    Some(s.iter().rev().fold(0, |v, &b| v << 8 | b as u64))
}

/// Whether `m` looks like a method id: `-` two letters or digits, one more, `-`.
fn method_like(m: &[u8]) -> bool {
    m.len() == 5 && m[0] == b'-' && m[4] == b'-' && m[1..4].iter().all(|b| b.is_ascii_alphanumeric())
}

/// Reads the header at `at`: the entry, or None where there is none.
pub fn header(d: &[u8], at: usize) -> Option<Entry> {
    let h = d.get(at..at + 22)?;
    if !method_like(&h[2..7]) {
        return None;
    }
    let method: [u8; 5] = h[2..7].try_into().unwrap();
    let level = h[20];
    let (packed, size) = (le(d, at + 7, 4)?, le(d, at + 11, 4)?);
    // the extended headers from `p`, the first of `next` bytes, their sizes of `w` bytes; up to
    // `limit` where it is known: their end, the name of type 1, and the sizes of type 0x42 (64
    // bits each, packed and original)
    type Ext = (usize, Option<Range<usize>>, Option<(u64, u64)>);
    let ext = |mut p: usize, mut next: u64, w: usize, limit: Option<usize>| -> Option<Ext> {
        let (mut name, mut sizes) = (None, None);
        while next != 0 {
            let n = usize::try_from(next).ok()?;
            if n < 1 + w {
                return None;
            }
            let end = p.checked_add(n)?;
            if limit.is_some_and(|l| end > l) || end > d.len() {
                return None;
            }
            match d[p] {
                1 => name = Some(p + 1..end - w),
                0x42 if n >= 17 + w => sizes = Some((le(d, p + 1, 8)?, le(d, p + 9, 8)?)),
                _ => {}
            }
            next = le(d, end - w, w)?;
            p = end;
        }
        Some((p, name, sizes))
    };
    let (header, name, crc, data_len, size) = match level {
        0 | 1 => {
            let len = h[0] as usize + 2;
            let n = h[21] as usize;
            if len < 24 + n + (level == 1) as usize * 3 || at + len > d.len() {
                return None;
            }
            let sum = d[at + 2..at + len].iter().fold(0u8, |s, &b| s.wrapping_add(b));
            if sum != h[1] {
                return None;
            }
            let crc = le(d, at + 22 + n, 2)? as u16;
            let name = at + 22..at + 22 + n;
            if level == 0 {
                (at..at + len, name, crc, packed, size)
            } else {
                let (end, ext_name, _) = ext(at + len, le(d, at + len - 2, 2)?, 2, None)?;
                let data_len = packed.checked_sub((end - at - len) as u64)?;
                (at..end, ext_name.unwrap_or(name), crc, data_len, size)
            }
        }
        2 => {
            let len = le(d, at, 2)? as usize;
            if len < 26 || at + len > d.len() {
                return None;
            }
            let (end, name, sizes) = ext(at + 26, le(d, at + 24, 2)?, 2, Some(at + len))?;
            if end > at + len {
                return None;
            }
            let (packed, size) = sizes.unwrap_or((packed, size));
            (at..at + len, name.unwrap_or(at..at), le(d, at + 21, 2)? as u16, packed, size)
        }
        3 => {
            if le(d, at, 2)? != 4 {
                return None;
            }
            let len = usize::try_from(le(d, at + 24, 4)?).ok()?;
            if len < 32 || at.checked_add(len)? > d.len() {
                return None;
            }
            let (end, name, sizes) = ext(at + 32, le(d, at + 28, 4)?, 4, Some(at + len))?;
            if end > at + len {
                return None;
            }
            let (packed, size) = sizes.unwrap_or((packed, size));
            (at..at + len, name.unwrap_or(at..at), le(d, at + 21, 2)? as u16, packed, size)
        }
        _ => return None,
    };
    let start = header.end;
    let data = start..start.checked_add(usize::try_from(data_len).ok()?)?;
    if data.end > d.len() {
        return None;
    }
    Some(Entry { header, level, method, name, size, crc, data })
}

/// Whether `d` starts with an LHA header.
pub fn starts(d: &[u8]) -> bool {
    header(d, 0).is_some()
}

/// Reads the archive at the start of `d`. None where it does not start with a header.
pub fn parse(d: &[u8]) -> Option<Lha> {
    let mut entries = Vec::new();
    let mut at = 0;
    let end = loop {
        if at >= d.len() {
            break d.len();
        }
        if d[at] == 0 {
            break at + 1;
        }
        match header(d, at) {
            Some(e) => {
                at = e.data.end;
                entries.push(e);
            }
            None => break at,
        }
    };
    (!entries.is_empty()).then_some(Lha { entries, end })
}

/// The CRC-16 of the files (ARC's: reflected polynomial 0xA001, from 0).
pub fn crc16(d: &[u8]) -> u16 {
    static TABLE: std::sync::OnceLock<[u16; 256]> = std::sync::OnceLock::new();
    let t = TABLE.get_or_init(|| {
        let mut t = [0u16; 256];
        for (i, e) in t.iter_mut().enumerate() {
            let mut c = i as u16;
            for _ in 0..8 {
                c = if c & 1 != 0 { c >> 1 ^ 0xA001 } else { c >> 1 };
            }
            *e = c;
        }
        t
    });
    d.iter().fold(0u16, |c, &b| c >> 8 ^ t[((c ^ b as u16) & 0xff) as usize])
}
