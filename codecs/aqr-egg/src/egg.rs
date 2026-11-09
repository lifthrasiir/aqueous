//! EGG archives read: their records one after another from the start (`FORMAT.md`, "The
//! archive").

use std::ops::Range;

/// The archive's header: its signature (a version, 2 bytes, an id and a reserved word, 4 bytes
/// each, follow).
pub const MAGIC: [u8; 4] = *b"EGGA";
/// Bytes of the archive's header.
pub const HEADER: usize = 14;

const SPLIT: u32 = 0x24F5_A262;
const SOLID: u32 = 0x24E5_A060;
const GLOBAL_ENCRYPT: u32 = 0x08D1_44A8;
const SKIP: u32 = 0xFFFF_0000;
const FILE: u32 = 0x0A85_90E3;
const FILENAME: u32 = 0x0A85_91AC;
const COMMENT: u32 = 0x04C6_3672;
const WINDOWS_INFO: u32 = 0x2C86_950B;
const POSIX_INFO: u32 = 0x1EE9_22E5;
const ENCRYPT: u32 = 0x08D1_470F;
const BLOCK: u32 = 0x02B5_0C13;
const DUMMY: u32 = 0x0746_3307;
const END: u32 = 0x08E2_8222;

/// Bytes of a filename's locale (a code page), where its flags' bit 3 is set.
const LOCALE: usize = 2;

/// The methods of a block.
pub const STORED: u8 = 0;
pub const DEFLATE: u8 = 1;
pub const BZIP2: u8 = 2;
pub const AZO: u8 = 3;
pub const LZMA: u8 = 4;

/// Bytes of the header of an LZMA block's data before its stream: 4 bytes the decoders skip and
/// LZMA's properties (5 bytes).
pub const LZMA_HEADER: usize = 9;

/// A block of a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    pub method: u8,
    /// The uncompressed size and the CRC-32, as the header says.
    pub size: u32,
    pub crc: u32,
    /// The compressed data.
    pub body: Range<usize>,
}

/// A file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Where its header starts.
    pub pos: usize,
    /// The name, of its first filename record (empty where it has none), and its locale (a code
    /// page, 0 the system's) where it has one, else UTF-8.
    pub name: Range<usize>,
    pub locale: Option<u16>,
    /// Of its Windows file information (spec §8.3): the modification time (a FILETIME) and the
    /// attributes (Windows' read-only, hidden and system bits, 0x08 a symbolic link, 0x80 a
    /// directory).
    pub windows: Option<(u64, u8)>,
    /// Of its POSIX file information (spec §8.4).
    pub posix: Option<Posix>,
    /// Whether it has an encryption record.
    pub encrypted: bool,
    /// The uncompressed size, as the header says.
    pub size: u64,
    pub blocks: Vec<Block>,
}

/// A file's POSIX information: its mode (of `stat`), owner, group and modification time (seconds
/// since 1970).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Posix {
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    pub mtime: u64,
}

/// An archive read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Egg {
    /// Whether it is solid (its blocks one stream of each method).
    pub solid: bool,
    /// Whether it is a volume of a split archive.
    pub split: bool,
    pub entries: Vec<Entry>,
    /// Where the records end: after the final end marker, at the first byte that is not a record,
    /// or at the end of the data where a record runs past it (a volume of a split archive, an
    /// archive cut short).
    pub end: usize,
    /// Whether the records end with the final end marker (else other bytes or the end of the
    /// data end them).
    pub ended: bool,
}

fn le(d: &[u8], at: usize, n: usize) -> Option<u64> {
    let b = d.get(at..at.checked_add(n)?)?;
    Some(b.iter().rev().fold(0, |a, &x| a << 8 | x as u64))
}

fn sig(d: &[u8], at: usize) -> Option<u32> {
    le(d, at, 4).map(|s| s as u32)
}

/// Whether `d` starts with an archive's header and a record after it.
pub fn starts(d: &[u8]) -> bool {
    d.starts_with(&MAGIC) && sig(d, HEADER).is_some_and(|s| [SPLIT, SOLID, SKIP, GLOBAL_ENCRYPT, END].contains(&s))
}

/// The record of an extra field at `p` (its signature checked): its flags, its data and where it
/// ends, or None where it runs past `d`.
fn extra(d: &[u8], p: usize) -> Option<(u8, Range<usize>)> {
    let flags = *d.get(p + 4)?;
    let w = if flags & 1 != 0 { 4 } else { 2 };
    let n = usize::try_from(le(d, p + 5, w)?).ok()?;
    let at = p + 5 + w;
    let end = at.checked_add(n).filter(|&e| e <= d.len())?;
    Some((flags, at..end))
}

/// What reading records at a place came to.
enum Stop {
    /// A record that is none of those expected there, at its start.
    Other(usize),
    /// A record running past the data.
    Past,
}

/// The file at `p` (its signature checked): its header, its records up to their end marker and
/// its blocks.
fn file(d: &[u8], p: usize) -> Result<(Entry, usize), Stop> {
    let size = le(d, p + 8, 8).ok_or(Stop::Past)?;
    let mut e = Entry {
        pos: p,
        name: 0..0,
        locale: None,
        windows: None,
        posix: None,
        encrypted: false,
        size,
        blocks: Vec::new(),
    };
    let mut at = p + 16;
    let mut named = false;
    loop {
        match sig(d, at).ok_or(Stop::Past)? {
            END => break at += 4,
            BLOCK | FILE => return Err(Stop::Other(p)),
            s => {
                // (a filename, a comment, file information, an encryption record, or another
                // record of an extra field, as the spec's §16.5 reads them)
                let (flags, mut r) = extra(d, at).ok_or(Stop::Past)?;
                if s == FILENAME {
                    // (a filename's bit 3 tells of a locale, 2 bytes before the name that its
                    // size does not count: FORMAT.md, "The archive")
                    let mut locale = None;
                    if flags & 8 != 0 {
                        locale = le(d, r.start, LOCALE).map(|l| l as u16);
                        r = r.start + LOCALE..r.end + LOCALE;
                        if r.end > d.len() {
                            return Err(Stop::Past);
                        }
                    }
                    if !named {
                        (e.name, e.locale) = (r.clone(), locale);
                        named = true;
                    }
                } else if s == WINDOWS_INFO && r.len() >= 9 {
                    e.windows = Some((le(d, r.start, 8).unwrap(), d[r.start + 8]));
                } else if s == POSIX_INFO && r.len() >= 20 {
                    let at = |k, n| le(d, r.start + k, n).unwrap();
                    e.posix = Some(Posix {
                        mode: at(0, 4) as u32,
                        uid: at(4, 4) as u32,
                        gid: at(8, 4) as u32,
                        mtime: at(12, 8),
                    });
                }
                e.encrypted |= s == ENCRYPT;
                at = r.end;
            }
        }
    }
    while sig(d, at) == Some(BLOCK) {
        let h = at + 4;
        let (method, size, csize, crc) = (
            *d.get(h).ok_or(Stop::Past)?,
            le(d, h + 2, 4).ok_or(Stop::Past)? as u32,
            le(d, h + 6, 4).ok_or(Stop::Past)? as usize,
            le(d, h + 10, 4).ok_or(Stop::Past)? as u32,
        );
        match sig(d, h + 14) {
            None => return Err(Stop::Past),
            Some(END) => {}
            Some(_) => return Err(Stop::Other(p)),
        }
        let start = h + 18;
        let end = start.checked_add(csize).filter(|&x| x <= d.len()).ok_or(Stop::Past)?;
        e.blocks.push(Block { method, size, crc, body: start..end });
        at = end;
    }
    Ok((e, at))
}

/// The archive at the start of `d`: its records from its header on. None where it does not start
/// with one ([`starts`]).
pub fn parse(d: &[u8]) -> Option<Egg> {
    if !starts(d) {
        return None;
    }
    let mut egg = Egg { solid: false, split: false, entries: Vec::new(), end: d.len(), ended: false };
    // the prefix's records up to their end marker
    let mut p = HEADER;
    loop {
        match sig(d, p) {
            Some(END) => break p += 4,
            Some(s @ (SPLIT | SOLID | SKIP | GLOBAL_ENCRYPT)) => match extra(d, p) {
                Some((_, r)) => {
                    egg.solid |= s == SOLID;
                    egg.split |= s == SPLIT;
                    p = r.end;
                }
                None => return Some(egg),
            },
            Some(_) => {
                egg.end = p;
                return Some(egg);
            }
            None => return Some(egg),
        }
    }
    egg.end = loop {
        let next = match sig(d, p) {
            None => break d.len(),
            Some(FILE) => match file(d, p) {
                Ok((e, end)) => {
                    egg.entries.push(e);
                    Some(end)
                }
                Err(Stop::Other(at)) => break at,
                Err(Stop::Past) => None,
            },
            Some(COMMENT | DUMMY) => extra(d, p).map(|(_, r)| r.end),
            Some(END) => {
                egg.ended = true;
                break p + 4;
            }
            Some(_) => break p,
        };
        match next {
            Some(n) => p = n,
            // (a record running past the data: the archive goes on in the next volume, or was
            // cut short)
            None => break d.len(),
        }
    };
    Some(egg)
}

/// The CRC-32 and size of the files' contents in the archive whose bytes `read` gives by their
/// span (the records alone, one after another from the start, the blocks' data skipped): of the
/// files of one block of all of their data, not encrypted, up to the first record that is not a
/// file's. None of a solid archive's (its blocks hold several files).
pub fn entry_sums(read: &mut dyn FnMut(Range<u64>) -> Option<Vec<u8>>) -> Vec<(u32, u64)> {
    let mut read_le =
        |at: u64, n: usize| read(at..at + n as u64).filter(|d| d.len() == n).map(|d| le(&d, 0, n).unwrap());
    let mut sums = Vec::new();
    if read_le(0, 4) != Some(u32::from_le_bytes(MAGIC) as u64) {
        return sums;
    }
    // the flags of the record of an extra field at `p`, and where it ends
    fn extra_end(read_le: &mut dyn FnMut(u64, usize) -> Option<u64>, p: u64) -> Option<(u8, u64)> {
        let flags = read_le(p + 4, 1)? as u8;
        let w = if flags & 1 != 0 { 4 } else { 2 };
        Some((flags, (p + 5 + w as u64).checked_add(read_le(p + 5, w)?)?))
    }
    let mut p = HEADER as u64;
    loop {
        match read_le(p, 4).map(|s| s as u32) {
            Some(END) => break p += 4,
            Some(SOLID) | None => return sums,
            Some(SPLIT | SKIP | GLOBAL_ENCRYPT) => match extra_end(&mut read_le, p) {
                Some((_, e)) => p = e,
                None => return sums,
            },
            Some(_) => return sums,
        }
    }
    'files: while read_le(p, 4) == Some(FILE as u64) {
        let Some(size) = read_le(p + 8, 8) else { break };
        let (mut at, mut encrypted) = (p + 16, false);
        loop {
            match read_le(at, 4).map(|s| s as u32) {
                Some(END) => break at += 4,
                None | Some(BLOCK | FILE) => break 'files,
                Some(s) => {
                    let Some((flags, mut end)) = extra_end(&mut read_le, at) else { break 'files };
                    // (a filename's locale, which its size does not count)
                    if s == FILENAME && flags & 8 != 0 {
                        end += LOCALE as u64;
                    }
                    encrypted |= s == ENCRYPT;
                    at = end;
                }
            }
        }
        let mut blocks = Vec::new();
        while read_le(at, 4) == Some(BLOCK as u64) {
            let h = at + 4;
            let (Some(bsize), Some(csize), Some(crc)) = (read_le(h + 2, 4), read_le(h + 6, 4), read_le(h + 10, 4))
            else {
                break 'files;
            };
            blocks.push((crc as u32, bsize));
            at = h + 18 + csize;
            // (a block running past the data: of a volume of a split archive, or cut short)
            if read_le(at - 1, 1).is_none() {
                break 'files;
            }
        }
        if let ([(crc, n)], false) = (&blocks[..], encrypted)
            && *n == size
        {
            sums.push((*crc, size));
        }
        p = at;
    }
    sums
}

/// A file to [`fn@write`]: its name, its block's method, its compressed data and its size.
pub type Written<'a> = (&'a str, u8, &'a [u8], u32);

/// An archive of `files`, each of one block (of no CRC-32), solid or not, laid out as ALZip lays
/// them out: for tests.
pub fn write(solid: bool, files: &[Written<'_>]) -> Vec<u8> {
    let mut d = MAGIC.to_vec();
    d.extend_from_slice(&[0, 1, 1, 2, 3, 4, 0, 0, 0, 0]);
    let put = |d: &mut Vec<u8>, s: u32| d.extend_from_slice(&s.to_le_bytes());
    if solid {
        put(&mut d, SOLID);
        d.extend_from_slice(&[0, 0, 0]);
    }
    put(&mut d, END);
    for (i, &(name, method, body, size)) in files.iter().enumerate() {
        put(&mut d, FILE);
        put(&mut d, i as u32);
        d.extend_from_slice(&(size as u64).to_le_bytes());
        put(&mut d, FILENAME);
        d.push(0);
        d.extend_from_slice(&(name.len() as u16).to_le_bytes());
        d.extend_from_slice(name.as_bytes());
        put(&mut d, END);
        put(&mut d, BLOCK);
        d.extend_from_slice(&[method, 0]);
        put(&mut d, size);
        put(&mut d, body.len() as u32);
        put(&mut d, 0);
        put(&mut d, END);
        d.extend_from_slice(body);
    }
    put(&mut d, END);
    d
}

#[cfg(test)]
mod tests {
    use super::*;

    fn put(d: &mut Vec<u8>, s: u32) {
        d.extend_from_slice(&s.to_le_bytes());
    }

    #[test]
    fn records() {
        let mut d = MAGIC.to_vec();
        d.extend_from_slice(&[0, 1, 1, 2, 3, 4, 0, 0, 0, 0]);
        put(&mut d, SOLID);
        d.extend_from_slice(&[0, 0, 0]);
        put(&mut d, END);
        // a directory, then a stored file of 3 bytes (an extra field of a 4-byte size, a comment)
        put(&mut d, FILE);
        d.extend_from_slice(&[0; 12]);
        put(&mut d, FILENAME);
        d.extend_from_slice(b"\x00\x03\x00dir");
        put(&mut d, END);
        put(&mut d, FILE);
        d.extend_from_slice(&[1, 0, 0, 0, 3, 0, 0, 0, 0, 0, 0, 0]);
        put(&mut d, FILENAME);
        d.extend_from_slice(b"\x01\x01\x00\x00\x00a");
        put(&mut d, COMMENT);
        d.extend_from_slice(b"\x00\x02\x00hi");
        put(&mut d, END);
        put(&mut d, BLOCK);
        d.extend_from_slice(&[0, 0, 3, 0, 0, 0, 3, 0, 0, 0, 1, 2, 3, 4]);
        put(&mut d, END);
        d.extend_from_slice(b"abc");
        put(&mut d, COMMENT);
        d.extend_from_slice(b"\x00\x01\x00!");
        put(&mut d, END);
        let n = d.len();
        d.extend_from_slice(b"after");
        let a = parse(&d).unwrap();
        assert!(a.solid && !a.split && a.ended);
        assert_eq!(a.end, n);
        assert_eq!(a.entries.len(), 2);
        assert_eq!((a.entries[0].blocks.len(), &d[a.entries[0].name.clone()]), (0, &b"dir"[..]));
        let e = &a.entries[1];
        assert_eq!((e.size, &d[e.name.clone()]), (3, &b"a"[..]));
        let b = &e.blocks[0];
        assert_eq!((b.method, b.size, b.crc, &d[b.body.clone()]), (STORED, 3, 0x04030201, &b"abc"[..]));
        // cut short: the records run to the end; a block's header without its end marker: not a
        // file
        let cut = parse(&d[..n - 20]).unwrap();
        assert_eq!((cut.end, cut.ended), (n - 20, false));
        let mut bad = d.clone();
        let at = b.body.start - 4;
        bad[at] ^= 1;
        assert_eq!(parse(&bad).unwrap().end, e.pos);
        assert!(parse(b"EGGA\0\x01\0\0\0\0\0\0\0\0XXXX").is_none());
    }
}
