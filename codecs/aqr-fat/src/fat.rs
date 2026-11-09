//! FAT12, FAT16 and FAT32 (Microsoft's FAT specification, fatgen103): the BPB of the boot
//! sector, the first FAT, the directories from the root (FAT12's and FAT16's fixed region, or
//! FAT32's cluster), their entries' long names (VFAT) or else short ones. A file's contents are its
//! cluster chain up to its size.

use std::borrow::Cow;
use std::ops::Range;

use aqr_disk::Image;

use crate::{File, extents, le16, le32, utf16};

/// Deepest directory read, from the root.
const DEPTH: usize = 256;

/// Most bytes of a directory read: the 65536 entries a directory may have.
const DIRECTORY: usize = 65536 * 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Width {
    Fat12,
    Fat16,
    Fat32,
}

/// What the BPB tells of a volume.
#[derive(Debug, Clone)]
struct Volume {
    width: Width,
    /// Bytes of a cluster.
    cluster: usize,
    /// Where the first FAT is, and its bytes.
    fat: Range<usize>,
    /// FAT12's and FAT16's root directory (a region of its own), or FAT32's first cluster.
    root: Result<Range<usize>, u32>,
    /// Where cluster 2 is.
    heap: usize,
    /// Clusters of the data, numbered from 2.
    count: u32,
    /// Clusters still to be followed in all, and bytes of directories still to be read: of a
    /// volume whose chains meet or loop, as damaged or made ones do, the time stays that of the
    /// clusters and the bytes.
    budget: std::cell::Cell<(usize, usize)>,
}

/// The BPB of boot sector `d`, where it is one: a jump, the signature, sizes of the values the
/// specification allows, and a width that fits the count of clusters (FAT32's by its own fields).
fn volume(d: &dyn Image) -> Option<Volume> {
    let b = &*d.get(0..512)?;
    if !matches!(b[0], 0xeb | 0xe9) || b[510..512] != [0x55, 0xaa] {
        return None;
    }
    let bps = le16(b, 11) as usize;
    let spc = b[13] as usize;
    let (reserved, fats, entries) = (le16(b, 14) as usize, b[16] as usize, le16(b, 17) as usize);
    let sectors = match le16(b, 19) {
        0 => le32(b, 32) as usize,
        n => n as usize,
    };
    let fat_sectors = match le16(b, 22) {
        0 => le32(b, 36) as usize,
        n => n as usize,
    };
    if !matches!(bps, 512 | 1024 | 2048 | 4096)
        || !spc.is_power_of_two()
        || spc > 128
        || reserved == 0
        || !(1..=4).contains(&fats)
        || fat_sectors == 0
        || !(b[21] == 0xf0 || b[21] >= 0xf8)
    {
        return None;
    }
    let root_sectors = (entries * 32).div_ceil(bps);
    let first = reserved.checked_add(fats.checked_mul(fat_sectors)?)?.checked_add(root_sectors)?;
    let count = u32::try_from(sectors.checked_sub(first)? / spc).ok()?;
    let width = match count {
        0 => return None,
        1..4085 => Width::Fat12,
        4085..65525 => Width::Fat16,
        _ => Width::Fat32,
    };
    let fat = reserved * bps..(reserved + fat_sectors) * bps;
    let root = match width {
        Width::Fat32 if entries == 0 && le16(b, 22) == 0 => Err(le32(b, 44)),
        Width::Fat32 => return None,
        _ if entries == 0 => return None,
        _ => Ok((first - root_sectors) * bps..first * bps),
    };
    let budget = std::cell::Cell::new((2 * count as usize + 1024, d.len()));
    Some(Volume { width, cluster: bps * spc, fat, root, heap: first * bps, count, budget })
}

/// Whether `d` starts like a FAT volume (`volume`).
pub fn starts(d: &dyn Image) -> bool {
    volume(d).is_some()
}

impl Volume {
    /// The entry of cluster `n` in `fat` (the bytes of the first FAT, as many as there are), where it is within.
    fn next(&self, fat: &[u8], n: u32) -> Option<u32> {
        let n = n as usize;
        let (at, len) = match self.width {
            Width::Fat12 => (n + n / 2, 2),
            Width::Fat16 => (n * 2, 2),
            Width::Fat32 => (n * 4, 4),
        };
        let e = fat.get(at..at + len)?;
        Some(match self.width {
            Width::Fat12 if n % 2 == 1 => (le16(e, 0) >> 4) as u32,
            Width::Fat12 => (le16(e, 0) & 0xfff) as u32,
            Width::Fat16 => le16(e, 0) as u32,
            Width::Fat32 => le32(e, 0) & 0x0fff_ffff,
        })
    }

    /// The chain of clusters from `first`, at most `most` of them: up to its end, a bad cluster or
    /// a number of no cluster; a loop ends with the count of the clusters.
    fn chain(&self, fat: &[u8], first: u32, most: usize) -> Vec<u32> {
        let end = match self.width {
            Width::Fat12 => 0xff7,
            Width::Fat16 => 0xfff7,
            Width::Fat32 => 0x0fff_fff7,
        };
        let (steps, bytes) = self.budget.get();
        let most = most.min(self.count as usize).min(steps);
        let mut out = Vec::new();
        let mut c = first;
        while (2..=self.count + 1).contains(&c) && out.len() < most {
            out.push(c);
            match self.next(fat, c) {
                Some(n) if n < end => c = n,
                _ => break,
            }
        }
        self.budget.set((steps - out.len(), bytes));
        out
    }

    /// The bytes of the directory of first cluster `first` (or the root's region), as many as
    /// there are of it.
    fn directory(&self, d: &dyn Image, fat: &[u8], first: Option<u32>) -> Vec<u8> {
        let first = match (first, &self.root) {
            (Some(c), _) | (None, &Err(c)) => c,
            (None, Ok(r)) => return d.cut(r.clone()).into_owned(),
        };
        let mut out = Vec::new();
        for r in extents(&self.chain(fat, first, DIRECTORY / self.cluster), self.heap, self.cluster, DIRECTORY as u64) {
            let (steps, bytes) = self.budget.get();
            let bytes_of = d.cut(r.start..r.end.min(r.start.saturating_add(bytes)));
            self.budget.set((steps, bytes - bytes_of.len()));
            out.extend_from_slice(&bytes_of);
        }
        out
    }
}

/// The 8.3 name of short entry `e`: its base and extension trimmed, of lower case as Windows NT's
/// flags tell, each byte a character of Latin-1 (the OEM code page not known).
fn short_name(e: &[u8]) -> Vec<u8> {
    let part = |b: &[u8], lower: bool| -> String {
        let n = b.iter().rposition(|&c| c != b' ').map_or(0, |p| p + 1);
        b[..n].iter().map(|&c| if lower { c.to_ascii_lowercase() } else { c } as char).collect()
    };
    let mut base = e[..8].to_vec();
    if base[0] == 0x05 {
        base[0] = 0xe5;
    }
    let mut name = part(&base, e[12] & 0x08 != 0);
    let ext = part(&e[8..11], e[12] & 0x10 != 0);
    if !ext.is_empty() {
        name.push('.');
        name.push_str(&ext);
    }
    name.into_bytes()
}

/// A long name being read from its entries, the last first.
struct Long {
    units: Vec<u16>,
    sum: u8,
    /// The order of the entry wanted next (1 is the last), 0 once all are read.
    next: u8,
}

/// The files of FAT volume `d`.
pub fn files(d: &dyn Image) -> Vec<File> {
    let Some(v) = volume(d) else { return Vec::new() };
    // (the first FAT, once: of a container every read puts its bytes together)
    let fat: Cow<[u8]> = d.cut(v.fat.clone());
    let mut out = Vec::new();
    // (the directories to read: their path and first cluster, None of the root region)
    let mut todo: Vec<(Vec<u8>, Option<u32>, usize)> = vec![(Vec::new(), None, 0)];
    let mut seen = std::collections::HashSet::new();
    if let Err(c) = v.root {
        seen.insert(c);
    }
    while let Some((path, first, depth)) = todo.pop() {
        let dir = v.directory(d, &fat, first);
        let mut long: Option<Long> = None;
        for e in dir.as_chunks::<32>().0 {
            let e = &e[..];
            match e[0] {
                0 => break,
                0xe5 => {
                    long = None;
                    continue;
                }
                _ => {}
            }
            let attr = e[11];
            if attr & 0x3f == 0x0f {
                // a long name's entry: 13 units, the last entry first (0x40)
                let order = e[0] & 0x1f;
                let units =
                    || (1..11).step_by(2).chain((14..26).step_by(2)).chain((28..32).step_by(2)).map(|p| le16(e, p));
                match &mut long {
                    _ if e[0] & 0x40 != 0 && order > 0 => {
                        let mut l = Long { units: vec![0xffff; order as usize * 13], sum: e[13], next: order };
                        l.units[(order as usize - 1) * 13..].iter_mut().zip(units()).for_each(|(u, v)| *u = v);
                        l.next = order - 1;
                        long = Some(l);
                    }
                    Some(l) if l.next == order && order > 0 && l.sum == e[13] => {
                        l.units[(order as usize - 1) * 13..][..13].iter_mut().zip(units()).for_each(|(u, v)| *u = v);
                        l.next = order - 1;
                    }
                    _ => long = None,
                }
                continue;
            }
            let long = long.take();
            if attr & 0x08 != 0 || e[0] == b'.' {
                continue;
            }
            let sum = e[..11].iter().fold(0u8, |s, &c| s.rotate_right(1).wrapping_add(c));
            let name = match long.filter(|l| l.next == 0 && l.sum == sum) {
                Some(l) => {
                    let n = l.units.iter().position(|&u| u == 0 || u == 0xffff).unwrap_or(l.units.len());
                    utf16(&l.units[..n])
                }
                None => short_name(e),
            };
            if name.is_empty() {
                continue;
            }
            let full = match path.is_empty() {
                true => name,
                false => [&path[..], b"/", &name].concat(),
            };
            let high = if v.width == Width::Fat32 { (le16(e, 20) as u32) << 16 } else { 0 };
            let cluster = high | le16(e, 26) as u32;
            if attr & 0x10 != 0 {
                if depth < DEPTH && cluster >= 2 && seen.insert(cluster) {
                    todo.push((full, Some(cluster), depth + 1));
                }
                continue;
            }
            let size = le32(e, 28) as u64;
            let most = size.div_ceil(v.cluster as u64) as usize;
            let chain = if size == 0 { Vec::new() } else { v.chain(&fat, cluster, most) };
            out.push(File { name: full, extents: extents(&chain, v.heap, v.cluster, size), rank: 0, compressed: None });
        }
    }
    out
}
