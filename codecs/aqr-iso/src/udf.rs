//! UDF (ECMA-167 as OSTA UDF profiles it) directory trees: from the anchor at sector 256 (or
//! the last sector), the main volume descriptor sequence gives the partitions and the logical
//! volume, whose file set descriptor gives the root directory. Files are (extended) file
//! entries, their data in the extents of their allocation descriptors (short, long, chained by
//! allocation extent descriptors) or inline. Partitions of type 1 maps only: a virtual,
//! sparable or metadata partition (UDF 1.50 on, rare on pressed discs) is not read, and the
//! files in it are not found.

use std::collections::HashSet;
use std::ops::Range;

use crate::{File, SECTOR, le16, le32};

/// A descriptor of tag `id` at byte `at` of `d`, by its tag's checksum: the descriptor's bytes
/// from there on.
fn tag(d: &[u8], at: usize, id: u16) -> Option<&[u8]> {
    let t = d.get(at..)?.get(..16)?;
    let sum = t.iter().enumerate().filter(|&(i, _)| i != 4).fold(0u8, |s, (_, &b)| s.wrapping_add(b));
    (le16(t, 0) == id && sum == t[4]).then(|| &d[at..])
}

/// Where the logical blocks of each partition reference number start, if read.
struct Volume {
    parts: Vec<Option<usize>>,
}

impl Volume {
    /// Byte of logical block `lbn` of partition reference `part`.
    fn at(&self, part: u16, lbn: u32) -> Option<usize> {
        let start = (*self.parts.get(part as usize)?)?;
        (start + lbn as usize).checked_mul(SECTOR)
    }
}

/// The files of the UDF file system of image `d`, if it has one.
pub fn files(d: &[u8]) -> Vec<File> {
    let last = (d.len() / SECTOR).saturating_sub(1);
    let Some(anchor) = [256, last].into_iter().find_map(|s| tag(d, s * SECTOR, 2).filter(|a| a.len() >= 24)) else {
        return Vec::new();
    };
    let (len, loc) = (le32(anchor, 16) as usize, le32(anchor, 20) as usize);
    let (mut numbers, mut lvd) = (Vec::new(), None);
    // the main sequence: its partitions by number, and its logical volume
    for s in loc..loc + (len / SECTOR).min(64) {
        let at = s * SECTOR;
        let Some(&id) = d.get(at..at + 2).map(|t| &t[0]) else { break };
        match (id, tag(d, at, id as u16)) {
            (5, Some(p)) if p.len() >= 196 => numbers.push((le16(p, 22), le32(p, 188) as usize)),
            (6, Some(l)) if lvd.is_none() && l.len() >= 440 => lvd = Some(l),
            (8, Some(_)) => break,
            _ => {}
        }
    }
    let Some(lvd) = lvd.filter(|l| le32(l, 212) as usize == SECTOR) else { return Vec::new() };
    // the partition maps, of type 1 only
    let (maps, mut parts, mut p) = (le32(lvd, 268) as usize, Vec::new(), 440);
    for _ in 0..maps.min(64) {
        let Some(m) = lvd.get(p..p + 2).and_then(|h| lvd.get(p..p + (h[1] as usize).max(2))) else { break };
        parts.push(match m[0] {
            1 if m.len() >= 6 => numbers.iter().find(|n| n.0 == le16(m, 4)).map(|n| n.1),
            _ => None,
        });
        p += m.len();
    }
    let vol = Volume { parts };
    let fsd = long_ad(&lvd[248..264]);
    let Some(fsd) = vol.at(fsd.part, fsd.lbn).and_then(|at| tag(d, at, 256)).filter(|f| f.len() >= 416) else {
        return Vec::new();
    };
    let root = long_ad(&fsd[400..416]);
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let mut dirs = vec![(root, Vec::new())];
    while let Some((icb, path)) = dirs.pop() {
        if !seen.insert((icb.part, icb.lbn)) {
            continue;
        }
        let Some(dir) = entry(d, &vol, icb) else { continue };
        let data: Vec<u8> = match dir.inline {
            Some(r) => d[r].to_vec(),
            None => dir.extents.into_iter().flat_map(|r| &d[r]).copied().collect(),
        };
        let mut p = 0;
        while let Some(fid) = data.get(p..).and_then(|r| tag(r, 0, 257)).filter(|f| f.len() >= 38) {
            let (chars, l_fi, l_iu) = (fid[18], fid[19] as usize, le16(fid, 36) as usize);
            let Some(id) = fid.get(38 + l_iu..38 + l_iu + l_fi) else { break };
            p += (38 + l_iu + l_fi).div_ceil(4) * 4;
            // (the parent, and those deleted)
            if chars & 0x0c != 0 {
                continue;
            }
            let name = [&path[..], if path.is_empty() { b"" } else { b"/" }, &name(id)].concat();
            let icb = long_ad(&fid[20..36]);
            if chars & 2 != 0 {
                dirs.push((icb, name));
            } else if let Some(e) = entry(d, &vol, icb).filter(|e| e.inline.is_none()) {
                out.push(File { name, extents: e.extents, rank: 0 });
            }
        }
    }
    out
}

/// A long allocation descriptor's location.
#[derive(Clone, Copy)]
struct Long {
    lbn: u32,
    part: u16,
}

fn long_ad(b: &[u8]) -> Long {
    Long { lbn: le32(b, 4), part: le16(b, 8) }
}

/// A file's data in the image: its recorded extents (in bytes, up to its length), or the range
/// of the entry it is inline in.
struct Entry {
    extents: Vec<Range<usize>>,
    inline: Option<Range<usize>>,
}

/// The (extended) file entry of ICB `icb`, if it is one whose allocation descriptors are read.
fn entry(d: &[u8], vol: &Volume, icb: Long) -> Option<Entry> {
    let at = vol.at(icb.part, icb.lbn)?;
    let (fe, ad_at) = match tag(d, at, 261) {
        Some(fe) => (fe, 176),
        None => (tag(d, at, 266)?, 216),
    };
    let fe = fe.get(..SECTOR.min(fe.len()))?;
    let flags = le16(fe, 16 + 18);
    let info = u64::from_le_bytes(fe.get(56..64)?.try_into().unwrap());
    let (l_ea, l_ad) = (le32(fe, ad_at - 8) as usize, le32(fe, ad_at - 4) as usize);
    let ads = ad_at.checked_add(l_ea)?;
    let ads_end = ads.checked_add(l_ad)?;
    fe.get(ads..ads_end)?;
    let mut left = usize::try_from(info).ok()?;
    let (size, mut list) = match flags & 7 {
        0 => (8, &fe[ads..ads_end]),
        1 => (16, &fe[ads..ads_end]),
        3 => return (l_ad >= left).then(|| Entry { extents: Vec::new(), inline: Some(at + ads..at + ads + left) }),
        _ => return None,
    };
    let mut extents: Vec<Range<usize>> = Vec::new();
    // (chained lists bounded, as a loop of them would be endless)
    let mut chained = 0;
    while left > 0 {
        let Some(ad) = list.get(..size) else { break };
        list = &list[size..];
        let (len, kind) = ((le32(ad, 0) & 0x3fff_ffff) as usize, le32(ad, 0) >> 30);
        if len == 0 {
            break;
        }
        let part = if size == 16 { le16(ad, 8) } else { icb.part };
        let start = vol.at(part, le32(ad, 4));
        match kind {
            3 => {
                chained += 1;
                let aed = start.filter(|_| chained <= 1024).and_then(|s| tag(d, s, 258))?;
                list = aed.get(24..24 + le32(aed, 20) as usize)?;
            }
            0 => {
                let n = len.min(left);
                // (what is past the end of an image cut short is not read)
                let r = start?..start?.saturating_add(n).min(d.len());
                if !r.is_empty() {
                    extents.push(r);
                }
                left -= n;
            }
            // (allocated or not, not recorded: zeros, not in the image)
            _ => left -= len.min(left),
        }
    }
    // (contiguous extents as one)
    let mut merged: Vec<Range<usize>> = Vec::with_capacity(extents.len());
    for r in extents {
        match merged.last_mut() {
            Some(m) if m.end == r.start => m.end = r.end,
            _ => merged.push(r),
        }
    }
    Some(Entry { extents: merged, inline: None })
}

/// A file identifier (OSTA compressed Unicode: 8 or 16 bits a character after a byte telling
/// which) as UTF-8.
fn name(id: &[u8]) -> Vec<u8> {
    let s: String = match id.split_first() {
        Some((16, r)) => char::decode_utf16(r.as_chunks::<2>().0.iter().map(|&c| u16::from_be_bytes(c)))
            .map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER))
            .collect(),
        Some((_, r)) => r.iter().map(|&b| b as char).collect(),
        None => String::new(),
    };
    s.into_bytes()
}
