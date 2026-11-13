//! UDF (ECMA-167 as OSTA UDF profiles it) directory trees: from the anchor at sector 256 (or
//! the last sector), the main volume descriptor sequence gives the partitions and the logical
//! volume, whose file set descriptor gives the root directory. Files are (extended) file
//! entries, their data in the extents of their allocation descriptors (short, long, chained by
//! allocation extent descriptors) or inline. Partitions of type 1 maps only: a virtual,
//! sparable or metadata partition (UDF 1.50 on, rare on pressed discs) is not read, and the
//! files in it are not found.

use std::borrow::Cow;
use std::collections::HashSet;
use std::ops::Range;

use aqr_disk::{Image, join_path, push_extent, utf16_to_utf8};

use crate::{File, SECTOR, le16, le32};

/// Whether `t` starts with the tag of a descriptor `id`, by its checksum.
fn is_tag(t: &[u8], id: u16) -> bool {
    let Some(t) = t.get(..16) else { return false };
    let sum = t.iter().enumerate().filter(|&(i, _)| i != 4).fold(0u8, |s, (_, &b)| s.wrapping_add(b));
    le16(t, 0) == id && sum == t[4]
}

/// A descriptor of tag `id` at byte `at` of `d`, by its tag's checksum: its bytes from there on,
/// up to `max` of them (fewer at the image's end).
fn tag(d: &dyn Image, at: usize, id: u16, max: usize) -> Option<Cow<'_, [u8]>> {
    is_tag(&d.get(at..at.checked_add(16)?)?, id).then(|| d.cut(at..at.saturating_add(max.max(16))))
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
pub fn files(d: &dyn Image) -> Vec<File> {
    let last = (d.len() / SECTOR).saturating_sub(1);
    let Some(anchor) = [256, last].into_iter().find_map(|s| tag(d, s * SECTOR, 2, 24).filter(|a| a.len() >= 24)) else {
        return Vec::new();
    };
    let (len, loc) = (le32(&anchor, 16) as usize, le32(&anchor, 20) as usize);
    let (mut numbers, mut lvd) = (Vec::new(), None);
    // the main sequence: its partitions by number, and its logical volume
    for s in loc..loc + (len / SECTOR).min(64) {
        let at = s * SECTOR;
        let Some(id) = d.get(at..at + 2).map(|t| t[0]) else { break };
        match (id, matches!(id, 5 | 6 | 8).then(|| tag(d, at, id as u16, 440 + 64 * 255)).flatten()) {
            (5, Some(p)) if p.len() >= 196 => numbers.push((le16(&p, 22), le32(&p, 188) as usize)),
            (6, Some(l)) if lvd.is_none() && l.len() >= 440 => lvd = Some(l),
            (8, Some(_)) => break,
            _ => {}
        }
    }
    let Some(lvd) = lvd.filter(|l| le32(l, 212) as usize == SECTOR) else { return Vec::new() };
    let lvd = &lvd[..];
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
    let Some(fsd) = vol.at(fsd.part, fsd.lbn).and_then(|at| tag(d, at, 256, 416)).filter(|f| f.len() >= 416) else {
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
            Some(r) => d.get(r).map(Cow::into_owned).unwrap_or_default(),
            None => gather(d, dir.extents),
        };
        let mut p = 0;
        while let Some(fid) = data.get(p..).filter(|r| is_tag(r, 257)).filter(|f| f.len() >= 38) {
            let (chars, l_fi, l_iu) = (fid[18], fid[19] as usize, le16(fid, 36) as usize);
            let Some(id) = fid.get(38 + l_iu..38 + l_iu + l_fi) else { break };
            p += (38 + l_iu + l_fi).div_ceil(4) * 4;
            // (the parent, and those deleted)
            if chars & 0x0c != 0 {
                continue;
            }
            let name = join_path(&path, name(id));
            let icb = long_ad(&fid[20..36]);
            if chars & 2 != 0 {
                dirs.push((icb, name));
            } else if let Some(e) = entry(d, &vol, icb).filter(|e| e.inline.is_none()) {
                out.push(File { name, extents: e.extents, rank: 0, compressed: None });
            }
        }
    }
    out
}

/// The bytes of `extents` of `d`, one after another, up to the image's length in all (a directory
/// holds no more, whatever its descriptors say).
fn gather(d: &dyn Image, extents: Vec<Range<usize>>) -> Vec<u8> {
    let mut data = Vec::new();
    for r in extents {
        if data.len() >= d.len() {
            break;
        }
        data.extend_from_slice(&d.cut(r));
    }
    data
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
fn entry(d: &dyn Image, vol: &Volume, icb: Long) -> Option<Entry> {
    let at = vol.at(icb.part, icb.lbn)?;
    let (fe, ad_at) = match tag(d, at, 261, SECTOR) {
        Some(fe) => (fe, 176),
        None => (tag(d, at, 266, SECTOR)?, 216),
    };
    let fe = &fe[..];
    // (cut short by the image's end)
    if fe.len() < ad_at {
        return None;
    }
    let flags = le16(fe, 16 + 18);
    let info = u64::from_le_bytes(fe.get(56..64)?.try_into().unwrap());
    let (l_ea, l_ad) = (le32(fe, ad_at - 8) as usize, le32(fe, ad_at - 4) as usize);
    let ads = ad_at.checked_add(l_ea)?;
    let ads_end = ads.checked_add(l_ad)?;
    fe.get(ads..ads_end)?;
    let mut left = usize::try_from(info).ok()?;
    let (size, mut list) = match flags & 7 {
        0 => (8, fe[ads..ads_end].to_vec()),
        1 => (16, fe[ads..ads_end].to_vec()),
        3 => return (l_ad >= left).then(|| Entry { extents: Vec::new(), inline: Some(at + ads..at + ads + left) }),
        _ => return None,
    };
    let mut extents: Vec<Range<usize>> = Vec::new();
    // (chained lists bounded, as a loop of them would be endless)
    let mut chained = 0;
    while left > 0 {
        let Some(head) = list.get(..size) else { break };
        let mut buf = [0u8; 16];
        buf[..size].copy_from_slice(head);
        let ad = &buf[..size];
        list.drain(..size);
        let (len, kind) = ((le32(ad, 0) & 0x3fff_ffff) as usize, le32(ad, 0) >> 30);
        if len == 0 {
            break;
        }
        let part = if size == 16 { le16(ad, 8) } else { icb.part };
        let start = vol.at(part, le32(ad, 4));
        match kind {
            3 => {
                chained += 1;
                // (an allocation extent descriptor is within a block)
                let aed = start.filter(|_| chained <= 1024).and_then(|s| tag(d, s, 258, SECTOR))?;
                list = aed.get(24..24 + le32(aed.get(..24)?, 20) as usize)?.to_vec();
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
        push_extent(&mut merged, r);
    }
    Some(Entry { extents: merged, inline: None })
}

/// A file identifier (OSTA compressed Unicode: 8 or 16 bits a character after a byte telling
/// which) as UTF-8.
fn name(id: &[u8]) -> Vec<u8> {
    match id.split_first() {
        Some((16, r)) => utf16_to_utf8(r.as_chunks::<2>().0.iter().map(|&c| u16::from_be_bytes(c))),
        Some((_, r)) => r.iter().map(|&b| b as char).collect::<String>().into_bytes(),
        None => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aqr_disk::Plain;

    /// A descriptor of tag `id` of `len` bytes, after `at` bytes of zeros.
    fn desc(at: usize, id: u16, len: usize) -> Vec<u8> {
        let mut d = vec![0u8; at + len];
        d[at..at + 2].copy_from_slice(&id.to_le_bytes());
        d[at + 4] = (id as u8).wrapping_add((id >> 8) as u8);
        d
    }

    /// Extents all of the same bytes do not make a directory many times the image.
    #[test]
    fn gather_of_repeated_extents() {
        let d = vec![7u8; 1000];
        let data = gather(&Plain(&d), vec![0..1000; 100_000]);
        assert!(data.len() <= 2 * d.len(), "{}", data.len());
        assert_eq!(gather(&Plain(&d), vec![0..10, 500..520]).len(), 30);
    }

    fn entry_at_0(d: &[u8], lbn: u32) -> Option<Entry> {
        entry(&Plain(d), &Volume { parts: vec![Some(0)] }, Long { lbn, part: 0 })
    }

    /// A file entry cut short by the image's end is not one.
    #[test]
    fn entry_cut_short() {
        for n in [16, 100, 175] {
            assert!(entry_at_0(&desc(0, 261, n), 0).is_none(), "{n}");
        }
    }

    /// An allocation extent descriptor cut short by the image's end is not one.
    #[test]
    fn allocation_extent_cut_short() {
        let mut d = desc(0, 261, SECTOR);
        d[56..64].copy_from_slice(&8u64.to_le_bytes());
        d[172..176].copy_from_slice(&8u32.to_le_bytes());
        d[176..180].copy_from_slice(&((3u32 << 30) | 1).to_le_bytes());
        d[180..184].copy_from_slice(&1u32.to_le_bytes());
        let mut aed = desc(0, 258, 20);
        d.append(&mut aed);
        assert!(entry_at_0(&d, 0).is_none());
    }
}
