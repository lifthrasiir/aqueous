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
    /// The host OS (0 MS-DOS, 1 OS/2, 2 Windows, 3 Unix, …) and the attributes (MS-DOS's or the
    /// Unix mode).
    pub host: u8,
    pub attrs: u32,
    /// The name's field holds a Unicode name ([`name`]).
    pub unicode: bool,
    /// The times: modification, creation and access, each an MS-DOS date and time (local; date
    /// in the high 16 bits) and nanoseconds past it, up to 2 s (of the extended times, their
    /// added second included; the modification time's MS-DOS time is the header's).
    pub mtime: Option<(u32, u32)>,
    pub ctime: Option<(u32, u32)>,
    pub atime: Option<(u32, u32)>,
}

#[derive(Debug)]
pub struct Rar4 {
    pub entries: Vec<Entry>,
    /// The end of the end block, or of the last block read.
    pub end: usize,
    pub solid: bool,
    pub volume: bool,
}

/// Whether `d` starts with a RAR 1.5–4.x archive: its marker and a main header of its CRC.
pub fn starts(d: &[u8]) -> bool {
    let at = SIGNATURE.len();
    let Some(h) = d.get(at..at + 7).filter(|_| d.starts_with(SIGNATURE)) else { return false };
    let size = le16(h, 5) as usize;
    h[2] == 0x73 && size >= 7 && d.get(at..at + size).is_some_and(|hd| crc32::hash(&hd[2..]) as u16 == le16(h, 0))
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
    // the extended times, after the name (and the salt)
    let dos = le32(hd, 20);
    let mut times = [Some((dos, 0u32)), None, None];
    if flags & 0x1000 != 0 {
        let mut q = p + nl + if flags & 0x0400 != 0 { 8 } else { 0 };
        let tf = le16(hd.get(q..q + 2)?, 0);
        q += 2;
        for k in 0..4 {
            let nib = (tf >> (12 - 4 * k)) & 15;
            if nib & 8 == 0 {
                continue;
            }
            let t = if k == 0 {
                dos
            } else {
                let t = le32(hd.get(q..q + 4)?, 0);
                q += 4;
                t
            };
            let m = (nib & 3) as usize;
            let b = hd.get(q..q + m)?;
            q += m;
            let frac = b.iter().enumerate().fold(0u32, |a, (i, &x)| a | (x as u32) << (8 * i)) << (8 * (3 - m));
            // (a second more where the flag says, past the MS-DOS time's 2-second step)
            let ns = (frac % 10_000_000) * 100 + if nib & 4 != 0 { 1_000_000_000 } else { 0 };
            // (the archive time, the fourth, not kept)
            if let Some(slot) = times.get_mut(k) {
                *slot = Some((t, ns));
            }
        }
    }
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
        host: hd[15],
        attrs: le32(hd, 28),
        unicode: flags & 0x0200 != 0,
        mtime: times[0],
        ctime: times[1],
        atime: times[2],
    })
}

/// The name of a field `f` of an entry: where `unicode`, the Unicode name it holds (UTF-8 where
/// it has no zero byte, else the plain name, a zero byte and the encoded one); else the field as
/// it is.
pub fn name(f: &[u8], unicode: bool) -> Vec<u8> {
    if !unicode {
        return f.to_vec();
    }
    let Some(z) = f.iter().position(|&b| b == 0) else { return f.to_vec() };
    let (plain, enc) = (&f[..z], &f[z + 1..]);
    let Some(&high) = enc.first() else { return plain.to_vec() };
    let high = high as u16;
    let mut out: Vec<u16> = Vec::new();
    let (mut pos, mut flags, mut bits) = (1usize, 0u8, 0);
    let limit = f.len();
    while pos < enc.len() && out.len() < limit {
        if bits == 0 {
            flags = enc[pos];
            pos += 1;
            bits = 8;
        }
        let op = flags >> 6;
        flags <<= 2;
        bits -= 2;
        match op {
            0 | 1 => {
                let Some(&b) = enc.get(pos) else { break };
                pos += 1;
                out.push(if op == 0 { b as u16 } else { high << 8 | b as u16 });
            }
            2 => {
                let Some(b) = enc.get(pos..pos + 2) else { break };
                pos += 2;
                out.push((b[1] as u16) << 8 | b[0] as u16);
            }
            _ => {
                let Some(&n) = enc.get(pos) else { break };
                pos += 1;
                let (c, hi) = if n & 0x80 != 0 {
                    let Some(&c) = enc.get(pos) else { break };
                    pos += 1;
                    (c, high)
                } else {
                    (0, 0)
                };
                for _ in 0..(n & 0x7f) as usize + 2 {
                    let Some(&p) = plain.get(out.len()) else { break };
                    out.push(hi << 8 | p.wrapping_add(c) as u16);
                }
            }
        }
    }
    String::from_utf16_lossy(&out).into_bytes()
}
