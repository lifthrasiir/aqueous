//! RAR 1.5–4.x archives as aqr-rar reads them (`docs/rar3-decoding.md` §2): the blocks in
//! turn, each file's and subblock's data, the solid streams.

use std::ops::Range;

use aqr_common::crc32;

pub const SIGNATURE: &[u8; 7] = b"Rar!\x1a\x07\x00";

/// A file header's or a subblock's data.
#[derive(Clone, Debug)]
pub struct Entry {
    /// A subblock (a comment, a recovery record, …).
    pub service: bool,
    pub name: Range<usize>,
    pub data: Range<usize>,
    pub size: u64,
    pub crc: u32,
    /// The version needed to unpack (29 for the algorithm of RAR 2.9 and later).
    pub version: u8,
    /// 0x30 stored, 0x31 to 0x35 compressed.
    pub method: u8,
    pub dict: u64,
    pub solid: bool,
    /// Continued from the volume before or in the next.
    pub split: bool,
    pub encrypted: bool,
    pub dir: bool,
}

#[derive(Debug)]
pub struct Rar4 {
    pub entries: Vec<Entry>,
    /// The end of the end block, or of the last block read.
    pub end: usize,
    pub solid: bool,
    pub volume: bool,
}

/// Whether `d` starts with a RAR 1.5–4.x archive.
pub fn starts(d: &[u8]) -> bool {
    d.starts_with(SIGNATURE)
}

fn le16(d: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([d[at], d[at + 1]])
}

fn le32(d: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(d[at..at + 4].try_into().unwrap())
}

/// Reads the archive at the start of `d`. None where it is none, or its headers are encrypted
/// or damaged before any file.
pub fn parse(d: &[u8]) -> Option<Rar4> {
    if !starts(d) {
        return None;
    }
    let mut at = SIGNATURE.len();
    let (mut entries, mut solid, mut volume) = (Vec::new(), false, false);
    let mut end = at;
    while let Some(h) = d.get(at..at + 7) {
        let (crc, kind, flags, size) = (le16(h, 0), h[2], le16(h, 3), le16(h, 5) as usize);
        let Some(hd) = d.get(at..at + size).filter(|_| size >= 7) else { break };
        if crc32::hash(&hd[2..]) as u16 != crc {
            break;
        }
        let mut next = at + size;
        match kind {
            0x73 => {
                solid = flags & 0x0008 != 0;
                volume = flags & 0x0001 != 0;
                if flags & 0x0080 != 0 {
                    break;
                }
            }
            0x74 | 0x7a => {
                let Some(e) = file(d, at, hd, flags, kind == 0x7a) else { break };
                next = e.data.end;
                entries.push(e);
            }
            0x7b => {
                end = next;
                break;
            }
            _ => {
                if flags & 0x8000 != 0 {
                    let Some(add) = hd.get(7..11) else { break };
                    let Some(n) = next.checked_add(le32(add, 0) as usize) else { break };
                    next = n;
                }
            }
        }
        if next > d.len() {
            break;
        }
        at = next;
        end = at;
    }
    if entries.is_empty() {
        return None;
    }
    Some(Rar4 { entries, end, solid, volume })
}

/// A file header or subblock `hd` at `at` of `d`, its data after it.
fn file(d: &[u8], at: usize, hd: &[u8], flags: u16, service: bool) -> Option<Entry> {
    if hd.len() < 32 {
        return None;
    }
    let (mut packed, mut size) = (le32(hd, 7) as u64, le32(hd, 11) as u64);
    let mut p = 32;
    if flags & 0x0100 != 0 {
        let hi = hd.get(32..40)?;
        packed |= (le32(hi, 0) as u64) << 32;
        size |= (le32(hi, 4) as u64) << 32;
        p = 40;
    }
    let nl = le16(hd, 26) as usize;
    let name = at + p..at + p + nl;
    if p + nl > hd.len() {
        return None;
    }
    let start = at + hd.len();
    let end = start.checked_add(usize::try_from(packed).ok()?)?;
    if end > d.len() {
        return None;
    }
    let n = (flags >> 5) & 7;
    Some(Entry {
        service,
        name,
        data: start..end,
        size,
        crc: le32(hd, 16),
        version: hd[24],
        method: hd[25],
        dict: if n == 7 { 0 } else { (64 << 10) << n },
        solid: flags & 0x0010 != 0,
        split: flags & 0x0003 != 0,
        encrypted: flags & 0x0004 != 0,
        dir: n == 7 && !service,
    })
}
