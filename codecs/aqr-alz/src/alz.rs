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
    let h = head(d, p)?;
    let f = p + 4;
    let name = h.at..h.at + h.nlen;
    let encrypted = h.desc & 1 != 0;
    let start = name.end;
    let end = usize::try_from(h.csize)
        .ok()
        .and_then(|n| n.checked_add(start + if encrypted { CRYPT_HEADER } else { 0 }))
        .filter(|&e| e <= d.len())
        .ok_or(true)?;
    let (attrs, time) = (d[f + 2], le(d, f + 3, 4).ok_or(true)? as u32);
    Ok(Entry { pos: p, name, attrs, time, method: h.method, encrypted, crc: h.crc, size: h.size, body: start..end })
}

/// What a local header at `p` tells (its signature checked), before its name.
struct Head {
    desc: u8,
    nlen: usize,
    method: Option<u8>,
    crc: u32,
    csize: u64,
    size: u64,
    /// Where its name starts.
    at: usize,
}

/// Bytes of a local header before its name, of a descriptor byte `desc`: None where it is not
/// one (sizes of another width).
fn head_len(desc: u8) -> Option<usize> {
    let width = match desc >> 4 {
        0 => 0,
        1 => 1,
        2 => 2,
        4 => 4,
        8 => 8,
        _ => return None,
    };
    Some(4 + FIXED + if width > 0 { 6 + 2 * width } else { 0 })
}

/// The local header at `p` (its signature checked): Err(true) where it runs past `d`, Err(false)
/// where it is not one.
fn head(d: &[u8], p: usize) -> Result<Head, bool> {
    let f = p + 4;
    let desc = *d.get(f + 7).ok_or(true)?;
    let len = head_len(desc).ok_or(false)?;
    let width = (len - 4 - FIXED).saturating_sub(6) / 2;
    let field = |at, n| le(d, at, n).ok_or(true);
    let nlen = field(f, 2)? as usize;
    let at = f + FIXED;
    let (mut method, mut crc, mut csize, mut size) = (None, 0, 0, 0);
    if width > 0 {
        method = Some(*d.get(at).ok_or(true)?);
        crc = field(at + 2, 4)? as u32;
        csize = field(at + 6, width)?;
        size = field(at + 6 + width, width)?;
    }
    Ok(Head { desc, nlen, method, crc, csize, size, at: p + len })
}

/// The CRC-32 and size of the files' contents in the archive whose bytes `read` gives by their
/// span (the local headers alone, one after another from the start): of the files not
/// encrypted, up to the first record that is not a local entry.
pub fn entry_sums(read: &mut dyn FnMut(Range<u64>) -> Option<Vec<u8>>) -> Vec<(u32, u64)> {
    let mut sums = Vec::new();
    if !read(0..HEADER as u64 + 4).is_some_and(|d| starts(&d)) {
        return sums;
    }
    let mut p = HEADER as u64;
    // (a desc of no sizes: a header of the fixed part alone)
    while let Some(fixed) = read(p..p + 4 + FIXED as u64).filter(|d| d.len() == 4 + FIXED && d[..4] == LOCAL) {
        let Some(len) = head_len(fixed[4 + 7]) else { break };
        let Some(Ok(h)) = read(p..p + len as u64).map(|d| head(&d, 0)) else { break };
        let encrypted = h.desc & 1 != 0;
        if h.method.is_some() && !encrypted {
            sums.push((h.crc, h.size));
        }
        let body = h.csize.saturating_add(if encrypted { CRYPT_HEADER as u64 } else { 0 });
        let end = p.saturating_add((h.at + h.nlen) as u64).saturating_add(body);
        // (a body running past the data: of a volume of a split archive, or cut short)
        if read(end - 1..end).is_none_or(|b| b.len() != 1) {
            if h.method.is_some() && !encrypted {
                sums.pop();
            }
            break;
        }
        p = end;
    }
    sums
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
