//! ALZ archives read: their records one after another from the start (`FORMAT.md`, "The
//! archive").

use std::ops::Range;

/// The archive's header: its signature (a version and an id follow, 2 bytes each).
pub const MAGIC: [u8; 4] = *b"ALZ\x01";
/// Bytes of the archive's header.
pub const HEADER: usize = 8;

const LOCAL: [u8; 4] = *b"BLZ\x01";
const CENTRAL: [u8; 4] = *b"CLZ\x01";
const END: [u8; 4] = *b"CLZ\x02";
const SPLIT: [u8; 4] = *b"CLZ\x03";
const COMMENT: [u8; 4] = *b"ELZ\x01";

/// The methods of an entry's body.
pub const STORED: u8 = 0;
pub const BZIP2: u8 = 1;
pub const DEFLATE: u8 = 2;

/// Bytes of the fixed part of a local header after its signature.
const FIXED: usize = 9;
/// Bytes of the encryption header before an encrypted body's data (ZIP's traditional
/// encryption), which the compressed size does not count.
pub const CRYPT_HEADER: usize = 12;

/// A local entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Where its local header starts.
    pub pos: usize,
    pub name: Range<usize>,
    /// The attributes (spec §4: Windows' read-only, hidden, system and archive bits, 0x10 a
    /// directory, 0x40 a symbolic link) and the modification time (MS-DOS's, its date in the
    /// high half).
    pub attrs: u8,
    pub time: u32,
    /// The method, None for an entry without sizes (a directory, an empty file).
    pub method: Option<u8>,
    pub encrypted: bool,
    /// The CRC-32 and the uncompressed size, as the header says (0 without sizes).
    pub crc: u32,
    pub size: u64,
    /// The body: the encryption header and the compressed data.
    pub body: Range<usize>,
}

/// An archive read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Alz {
    pub entries: Vec<Entry>,
    /// Where the records end: after the end record, at the first byte that is not a record, or
    /// at the end of the data where a record runs past it (a volume of a split archive, an
    /// archive cut short).
    pub end: usize,
    /// Whether the records end with the end record (its signature alone, or at the end of the
    /// central directory's 16 bytes; else a split marker, other bytes or the end of the data end
    /// them).
    pub ended: bool,
}

/// Whether `d` starts with an archive's header and a record after it.
pub fn starts(d: &[u8]) -> bool {
    d.starts_with(&MAGIC)
        && d.get(HEADER..HEADER + 4).is_some_and(|s| [LOCAL, CENTRAL, END, COMMENT].contains(&s.try_into().unwrap()))
}

fn le(d: &[u8], at: usize, n: usize) -> Option<u64> {
    let b = d.get(at..at.checked_add(n)?)?;
    Some(b.iter().rev().fold(0, |a, &x| a << 8 | x as u64))
}

/// The local entry at `p` (its signature checked): Err(true) where it runs past `d`, Err(false)
/// where it is not one (sizes of another width).
fn local(d: &[u8], p: usize) -> Result<Entry, bool> {
    let f = p + 4;
    let desc = *d.get(f + 7).ok_or(true)?;
    let width = match desc >> 4 {
        0 => 0,
        1 => 1,
        2 => 2,
        4 => 4,
        8 => 8,
        _ => return Err(false),
    };
    local_sized(d, p, desc, width).ok_or(true)
}

fn local_sized(d: &[u8], p: usize, desc: u8, width: usize) -> Option<Entry> {
    let f = p + 4;
    let nlen = le(d, f, 2)? as usize;
    let mut at = f + FIXED;
    let (mut method, mut crc, mut csize, mut size) = (None, 0, 0, 0);
    if width > 0 {
        method = Some(*d.get(at)?);
        crc = le(d, at + 2, 4)? as u32;
        csize = le(d, at + 6, width)?;
        size = le(d, at + 6 + width, width)?;
        at += 6 + 2 * width;
    }
    let name = at..at + nlen;
    let encrypted = desc & 1 != 0;
    let start = name.end;
    let end = usize::try_from(csize).ok()?.checked_add(start + if encrypted { CRYPT_HEADER } else { 0 })?;
    if end > d.len() {
        return None;
    }
    let (attrs, time) = (*d.get(f + 2)?, le(d, f + 3, 4)? as u32);
    Some(Entry { pos: p, name, attrs, time, method, encrypted, crc, size, body: start..end })
}

/// The comment section at `p` (its signature checked): its entries (a file's index, 4 bytes, the
/// comment's length, 2 bytes, and the comment) up to the central directory. Where it ends, or
/// None where it runs past `d`.
fn comments(d: &[u8], p: usize) -> Option<usize> {
    let mut at = p + 4;
    while d.get(at..at + 4)? != CENTRAL {
        at += 6 + le(d, at + 4, 2)? as usize;
    }
    Some(at)
}

/// The archive at the start of `d`: its records from its header on. None where it does not start
/// with one ([`starts`]).
pub fn parse(d: &[u8]) -> Option<Alz> {
    if !starts(d) {
        return None;
    }
    let mut entries = Vec::new();
    let mut p = HEADER;
    let mut ended = false;
    let end = loop {
        let Some(sig) = d.get(p..p + 4) else { break p.min(d.len()) };
        let next = match sig.try_into().unwrap() {
            LOCAL => match local(d, p) {
                Ok(e) => {
                    let end = e.body.end;
                    entries.push(e);
                    Some(end)
                }
                Err(false) => break p,
                Err(true) => None,
            },
            COMMENT => comments(d, p),
            CENTRAL => {
                // (its 16 bytes end with the end's signature in every archive seen)
                ended = d.get(p + 12..p + 16) == Some(&END[..]);
                Some(p + 16).filter(|&e| e <= d.len())
            }
            END => {
                ended = true;
                break p + 4;
            }
            SPLIT => break p + 4,
            _ => break p,
        };
        match next {
            Some(n) => p = n,
            // (a record running past the data: the archive goes on in the next volume, or was
            // cut short)
            None => break d.len(),
        }
    };
    Some(Alz { entries, end, ended })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records() {
        let mut d = MAGIC.to_vec();
        d.extend_from_slice(&[10, 0, 0, 0]);
        // a directory, then a stored file of 3 bytes of 1-byte sizes
        d.extend_from_slice(b"BLZ\x01\x03\x00\x10\0\0\0\0\x00\x00dir");
        d.extend_from_slice(b"BLZ\x01\x01\x00\x20\0\0\0\0\x10\x00");
        d.extend_from_slice(&[0, 0, 1, 2, 3, 4, 3, 3]);
        d.extend_from_slice(b"aabc");
        d.extend_from_slice(b"ELZ\x01\xff\xff\xff\xff\x02\x00hi");
        d.extend_from_slice(b"CLZ\x01");
        d.extend_from_slice(&[0; 12]);
        d.extend_from_slice(b"CLZ\x02");
        let n = d.len();
        d.extend_from_slice(b"after");
        let a = parse(&d).unwrap();
        assert_eq!((a.end, a.ended), (n, true));
        assert_eq!(a.entries.len(), 2);
        assert_eq!((a.entries[0].method, &d[a.entries[0].name.clone()]), (None, &b"dir"[..]));
        assert_eq!((a.entries[0].attrs, a.entries[1].attrs), (0x10, 0x20));
        let e = &a.entries[1];
        assert_eq!((e.method, e.crc, e.size, &d[e.body.clone()]), (Some(0), 0x04030201, 3, &b"abc"[..]));
        // cut short: the records run to the end; another width of sizes: not a record
        assert_eq!((parse(&d[..40]).unwrap().end, parse(&d[..40]).unwrap().ended), (40, false));
        let mut bad = d.clone();
        bad[24 + 4 + 7] = 0x30;
        assert_eq!(parse(&bad).unwrap().end, 24);
        assert!(parse(b"ALZ\x01\x0a\0\0\0XXXX").is_none());
    }
}
