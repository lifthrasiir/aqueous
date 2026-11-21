//! Compound files (OLE, [MS-CFB]; aqr-ole's `FS`): the bytes no reader reads, told by an own read
//! of the structures (not aqr-ole's, so that this and aqr-disk's listing are two reads):
//!
//! - the regular sectors the FAT marks free;
//! - in the last regular sector of a stream of the FAT (and of the root's mini stream), the bytes
//!   after the stream's size;
//! - the mini sectors the mini FAT marks free, and in the last mini sector of a mini stream the
//!   bytes after its size.
//!
//! Kept: the header, the FAT, DIFAT, mini FAT and directory sectors (the directory's unused
//! entries too), the sectors of every chain but their tails, a partial sector at the file's end.
//! Anything that does not hold together (a chain that loops, crosses another or leaves the
//! file, a sector of two chains or of none though not free, counts that disagree, a size no
//! chain fits) turns the file down.

use std::ops::Range;

const SIGNATURE: [u8; 8] = [0xd0, 0xcf, 0x11, 0xe0, 0xa1, 0xb1, 0x1a, 0xe1];
const FREE: u32 = 0xffff_ffff;
const END: u32 = 0xffff_fffe;
const FAT_SECTOR: u32 = 0xffff_fffd;
const DIFAT_SECTOR: u32 = 0xffff_fffc;
/// Entries of the DIFAT in the header.
const HEADER_DIFAT: usize = 109;
/// Bytes of a mini sector.
const MINI: usize = 64;

/// Who holds a sector.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Claim {
    None,
    Fat,
    Difat,
    Chain,
}

fn le16(b: &[u8], p: usize) -> u16 {
    u16::from_le_bytes([b[p], b[p + 1]])
}

fn le32(b: &[u8], p: usize) -> u32 {
    u32::from_le_bytes(b[p..p + 4].try_into().expect("4 bytes"))
}

/// The sectors of the chain from `start` in `fat`, each claimed (once: a loop or a crossing is
/// two claims); it must end by [`END`] without leaving the `claims`' sectors.
fn chain(fat: &[u32], claims: &mut [Claim], start: u32) -> Result<Vec<u32>, String> {
    let mut out = Vec::new();
    let mut s = start;
    loop {
        let c = claims.get_mut(s as usize).ok_or("a chain leaving the file")?;
        if *c != Claim::None {
            return Err("a chain looping, or crossing another".into());
        }
        *c = Claim::Chain;
        out.push(s);
        match fat[s as usize] {
            END => return Ok(out),
            n if (n as usize) < claims.len() => s = n,
            FREE => return Err("a chain through a free sector".into()),
            _ => return Err("a chain leaving the file, or through a sector of the tables".into()),
        }
    }
}

/// The free ranges of the compound file `v`.
pub(super) fn free(v: &[u8]) -> Result<Vec<Range<usize>>, String> {
    if v.len() < 512 || v[..8] != SIGNATURE || le16(v, 28) != 0xfffe || le16(v, 32) != 6 {
        return Err("not a compound file header".into());
    }
    let (version, ss) = match (le16(v, 26), le16(v, 30)) {
        (3, 9) => (3, 512),
        (4, 12) => (4, 4096),
        _ => return Err("unknown version or sector size".into()),
    };
    if v.len() < ss {
        return Err("a header cut short".into());
    }
    let (fat_count, dir_start, cutoff) = (le32(v, 44) as usize, le32(v, 48), le32(v, 56) as u64);
    let (mini_fat_start, mini_fat_count) = (le32(v, 60), le32(v, 64) as usize);
    let (difat_start, difat_count) = (le32(v, 68), le32(v, 72) as usize);
    if cutoff != 4096 {
        return Err("a mini stream cutoff other than 4096".into());
    }
    let nsec = (v.len() - ss) / ss;
    let at = |n: u32| (n as usize + 1) * ss;
    let sector = |n: u32| &v[at(n)..at(n) + ss];
    if nsec >= END as usize || fat_count == 0 || fat_count > nsec {
        return Err("sector counts of the header out of the file".into());
    }
    let mut claims = vec![Claim::None; nsec];

    // the FAT's sectors: the header's, then the DIFAT chain's
    let per = ss / 4 - 1;
    let mut listed: Vec<u32> = (0..HEADER_DIFAT).map(|i| le32(v, 76 + 4 * i)).collect();
    let want_difat = fat_count.saturating_sub(HEADER_DIFAT).div_ceil(per);
    if difat_count != want_difat {
        return Err("DIFAT sectors not as many as the FAT's need".into());
    }
    if difat_count == 0 {
        if difat_start != END && difat_start != FREE {
            return Err("a DIFAT start with no DIFAT sectors".into());
        }
    } else {
        let mut next = difat_start;
        for _ in 0..difat_count {
            let c = claims.get_mut(next as usize).ok_or("a DIFAT sector out of the file")?;
            if *c != Claim::None {
                return Err("a DIFAT sector claimed twice".into());
            }
            *c = Claim::Difat;
            let s = sector(next);
            listed.extend((0..per).map(|i| le32(s, 4 * i)));
            next = le32(s, 4 * per);
        }
        if next != END {
            return Err("a DIFAT chain not ended".into());
        }
    }
    if listed[fat_count..].iter().any(|&n| n != FREE) {
        return Err("FAT sectors listed beyond the header's count".into());
    }
    let mut fat: Vec<u32> = Vec::with_capacity(fat_count * ss / 4);
    for &n in &listed[..fat_count] {
        let c = claims.get_mut(n as usize).ok_or("a FAT sector out of the file")?;
        if *c != Claim::None {
            return Err("a FAT sector claimed twice".into());
        }
        *c = Claim::Fat;
        let s = sector(n);
        fat.extend((0..ss / 4).map(|i| le32(s, 4 * i)));
    }
    if fat.len() < nsec {
        return Err("a FAT not describing every sector".into());
    }

    // the directory
    if dir_start as usize >= nsec {
        return Err("a directory out of the file".into());
    }
    let dir_sectors = chain(&fat, &mut claims, dir_start)?;
    let mut entries: Vec<&[u8]> = Vec::new();
    for &n in &dir_sectors {
        entries.extend(sector(n).as_chunks::<128>().0.iter().map(|e| &e[..]));
    }
    // (type, start, size) of the streams (type 2) and the root (type 5, the first entry only)
    let mut root = None;
    let mut streams: Vec<(u32, u64)> = Vec::new();
    for (i, e) in entries.iter().enumerate() {
        let size = match version {
            3 if le32(e, 124) != 0 => return Err("a size beyond 32 bits in a version 3 file".into()),
            3 => le32(e, 120) as u64,
            _ => u64::from_le_bytes(e[120..128].try_into().expect("8 bytes")),
        };
        match (e[66], i) {
            (5, 0) => root = Some((le32(e, 116), size)),
            (0 | 1, _) => {}
            (2, _) if size > 0 => streams.push((le32(e, 116), size)),
            (2, _) => {}
            (5, _) => return Err("a root entry that is not the first".into()),
            _ => return Err("an unknown kind of directory entry".into()),
        }
    }
    let (root_start, root_size) = root.ok_or("no root entry")?;

    let mut out: Vec<Range<usize>> = Vec::new();
    // a stream of the FAT: exactly as many sectors as its size takes; returns them
    let mut regular = |start: u32, size: u64, out: &mut Vec<Range<usize>>| -> Result<Vec<u32>, String> {
        let need = size.div_ceil(ss as u64);
        if need == 0 {
            return Ok(Vec::new());
        }
        let sectors = chain(&fat, &mut claims, start)?;
        if sectors.len() as u64 != need {
            return Err("a stream whose chain is not as long as its size".into());
        }
        let used = (size - (need - 1) * ss as u64) as usize;
        let last = at(*sectors.last().expect("not empty"));
        out.push(last + used..last + ss);
        Ok(sectors)
    };
    let mini_stream = regular(root_start, root_size, &mut out)?;
    if root_size % MINI as u64 != 0 {
        return Err("a mini stream of a part of a mini sector".into());
    }
    let minis = (root_size / MINI as u64) as usize;
    for &(start, size) in streams.iter().filter(|s| s.1 >= cutoff) {
        regular(start, size, &mut out)?;
    }

    // the mini FAT
    let mut mini_fat: Vec<u32> = Vec::new();
    if mini_fat_count == 0 {
        if mini_fat_start != END && mini_fat_start != FREE {
            return Err("a mini FAT start with no mini FAT sectors".into());
        }
    } else {
        if mini_fat_start as usize >= nsec {
            return Err("a mini FAT out of the file".into());
        }
        let sectors = chain(&fat, &mut claims, mini_fat_start)?;
        if sectors.len() != mini_fat_count {
            return Err("a mini FAT not as long as its count".into());
        }
        for &n in &sectors {
            mini_fat.extend((0..ss / 4).map(|i| le32(sector(n), 4 * i)));
        }
    }
    if mini_fat.get(minis..).is_some_and(|rest| rest.iter().any(|&n| n != FREE)) {
        return Err("mini FAT entries past the mini stream".into());
    }
    if mini_fat.len() < minis {
        return Err("a mini FAT not describing every mini sector".into());
    }
    let mut mini_claims = vec![false; minis];
    let mini_at = |m: usize| -> usize { at(mini_stream[m * MINI / ss]) + m * MINI % ss };
    for &(start, size) in streams.iter().filter(|s| s.1 < cutoff) {
        let need = size.div_ceil(MINI as u64) as usize;
        let mut s = start as usize;
        let mut n = 0;
        loop {
            let c = mini_claims.get_mut(s).ok_or("a mini chain leaving the mini stream")?;
            if std::mem::replace(c, true) {
                return Err("a mini chain looping, or crossing another".into());
            }
            n += 1;
            match mini_fat[s] {
                END => break,
                m if (m as usize) < minis => s = m as usize,
                _ => return Err("a mini chain through a free sector, or leaving the mini stream".into()),
            }
        }
        if n != need {
            return Err("a mini stream whose chain is not as long as its size".into());
        }
        let at = mini_at(s);
        out.push(at + (size as usize - (need - 1) * MINI)..at + MINI);
    }
    for (m, &claimed) in mini_claims.iter().enumerate() {
        match (mini_fat[m], claimed) {
            (FREE, false) => out.push(mini_at(m)..mini_at(m) + MINI),
            (FREE, true) => return Err("a mini sector both free and in a chain".into()),
            (_, false) => return Err("a mini sector in no chain, not free".into()),
            _ => {}
        }
    }

    // every sector: free, or held, as the FAT says
    for (n, c) in claims.iter().enumerate() {
        match (fat[n], c) {
            (FREE, Claim::None) => out.push(at(n as u32)..at(n as u32) + ss),
            (FAT_SECTOR, Claim::Fat) | (DIFAT_SECTOR, Claim::Difat) => {}
            (FREE | FAT_SECTOR | DIFAT_SECTOR, _) => {
                return Err("a sector the FAT marks otherwise than it is held".into());
            }
            (_, Claim::Chain) => {}
            _ => return Err("a sector in no chain, not free".into()),
        }
    }

    out.retain(|r| !r.is_empty());
    out.sort_unstable_by_key(|r| r.start);
    let mut merged: Vec<Range<usize>> = Vec::with_capacity(out.len());
    for r in out {
        match merged.last_mut() {
            Some(l) if l.end > r.start => return Err("free ranges overlapping".into()),
            Some(l) if l.end == r.start => l.end = r.end,
            _ => merged.push(r),
        }
    }
    Ok(merged)
}
