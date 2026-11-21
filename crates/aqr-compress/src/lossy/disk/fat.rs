//! The free bytes of a volume of FAT12/16/32 and exFAT, as `--lossy=slack` zeroes them: the
//! clusters no file holds (a FAT entry of 0, an exFAT bit of 0 in the allocation bitmap) and the
//! tail of each file's last cluster past its size. Nothing else: not the boot sectors and
//! reserved sectors, the FATs, the bitmap and the up-case table, the directories (the root's
//! region, the clusters of directories, deleted entries and long names in them), bad clusters,
//! nor clusters marked used that nothing reaches (lost ones).
//!
//! The directory tree is walked from the root with a reader of its own (not aqr-fat's), every
//! chain followed and claimed: the volume is turned down where a chain loops, crosses another,
//! leaves the clusters or disagrees with its file's size, where the FATs differ or the bitmap
//! calls a cluster of a file free.

use std::ops::Range;

fn le16(b: &[u8], at: usize) -> u32 {
    u16::from_le_bytes([b[at], b[at + 1]]) as u32
}

fn le32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().expect("4 bytes"))
}

fn le64(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().expect("8 bytes"))
}

/// What a boot sector that is none is called.
const NEVER: &str = "invalid";

/// Sorts `r` and joins ranges that meet.
fn merged(mut r: Vec<Range<usize>>) -> Vec<Range<usize>> {
    r.retain(|x| x.start < x.end);
    r.sort_unstable_by_key(|x| x.start);
    let mut out: Vec<Range<usize>> = Vec::with_capacity(r.len());
    for x in r {
        match out.last_mut() {
            Some(l) if x.start <= l.end => l.end = l.end.max(x.end),
            _ => out.push(x),
        }
    }
    out
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Width {
    Fat12,
    Fat16,
    Fat32,
}

/// What the boot sector tells of a FAT volume.
struct Vol<'a> {
    d: &'a [u8],
    width: Width,
    /// Bytes of a cluster.
    cluster: usize,
    /// Where cluster 2 is.
    heap: usize,
    /// Clusters of the data, numbered from 2.
    count: u32,
    /// The first FAT's bytes.
    fat: &'a [u8],
    /// FAT12's and FAT16's root region, or FAT32's first cluster.
    root: Result<Range<usize>, u32>,
    /// Which clusters a file or directory holds.
    owned: Vec<bool>,
}

impl<'a> Vol<'a> {
    fn new(d: &'a [u8]) -> Result<Vol<'a>, String> {
        let bad = || format!("{NEVER} boot sector");
        let b = d.get(..512).ok_or_else(bad)?;
        if !matches!(b[0], 0xeb | 0xe9) || b[510..512] != [0x55, 0xaa] {
            return Err(bad());
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
            || !(1..=2).contains(&fats)
            || fat_sectors == 0
            || sectors == 0
            || !(b[21] == 0xf0 || b[21] >= 0xf8)
        {
            return Err(bad());
        }
        let root_sectors = (entries * 32).div_ceil(bps);
        let first = reserved + fats * fat_sectors + root_sectors;
        let count = sectors.checked_sub(first).map(|n| n / spc).filter(|&n| n > 0).ok_or_else(bad)?;
        let count = u32::try_from(count).map_err(|_| bad())?;
        let width = match count {
            1..4085 => Width::Fat12,
            4085..65525 => Width::Fat16,
            _ => Width::Fat32,
        };
        let root = match width {
            Width::Fat32 if entries == 0 && le16(b, 22) == 0 => Err(le32(b, 44)),
            Width::Fat32 => return Err(bad()),
            _ if entries == 0 => return Err(bad()),
            _ => Ok((first - root_sectors) * bps..first * bps),
        };
        let cluster = bps * spc;
        let heap = first * bps;
        if sectors * bps > d.len() {
            return Err("volume shorter than its boot sector tells".into());
        }
        if heap + count as usize * cluster > sectors * bps {
            return Err(bad());
        }
        // (the FATs: each of the sectors, all alike, with an entry for every cluster)
        let fat_bytes = fat_sectors * bps;
        let fat_at = |k: usize| (reserved + k * fat_sectors) * bps;
        let fat = &d[fat_at(0)..fat_at(0) + fat_bytes];
        for k in 1..fats {
            if d[fat_at(k)..fat_at(k) + fat_bytes] != *fat {
                return Err("FATs differing".into());
            }
        }
        let need = match width {
            Width::Fat12 => (count as usize + 2) * 3 / 2 + 1,
            Width::Fat16 => (count as usize + 2) * 2,
            Width::Fat32 => (count as usize + 2) * 4,
        };
        if fat_bytes < need {
            return Err("FAT shorter than its clusters".into());
        }
        Ok(Vol { d, width, cluster, heap, count, fat, root, owned: vec![false; count as usize + 2] })
    }

    /// The FAT's entry of cluster `n` (of 0 to the count + 1).
    fn entry(&self, n: u32) -> u32 {
        let n = n as usize;
        match self.width {
            Width::Fat12 => {
                let e = le16(self.fat, n + n / 2);
                if n % 2 == 1 { e >> 4 } else { e & 0xfff }
            }
            Width::Fat16 => le16(self.fat, n * 2),
            Width::Fat32 => le32(self.fat, n * 4) & 0x0fff_ffff,
        }
    }

    /// The entry from which a chain ends, and the entry of a bad cluster.
    fn eoc(&self) -> u32 {
        match self.width {
            Width::Fat12 => 0xff8,
            Width::Fat16 => 0xfff8,
            Width::Fat32 => 0x0fff_fff8,
        }
    }

    fn at(&self, c: u32) -> Range<usize> {
        let s = self.heap + (c as usize - 2) * self.cluster;
        s..s + self.cluster
    }

    /// The chain from `first`, claimed (and `n` clusters long where it is given).
    fn follow(&mut self, first: u32, n: Option<usize>) -> Result<Vec<u32>, String> {
        let mut out = Vec::new();
        let mut c = first;
        loop {
            if !(2..=self.count + 1).contains(&c) {
                return Err("a chain past the clusters".into());
            }
            if std::mem::replace(&mut self.owned[c as usize], true) {
                return Err("a cluster reached twice (a chain looping or crossing)".into());
            }
            out.push(c);
            if n.is_some_and(|n| out.len() > n) {
                return Err("a chain longer than its file".into());
            }
            let e = self.entry(c);
            if e >= self.eoc() {
                break;
            }
            if e == 0 {
                return Err("a chain through a free cluster".into());
            }
            if e == self.eoc() - 1 {
                return Err("a chain through a bad cluster".into());
            }
            c = e;
        }
        if n.is_some_and(|n| out.len() != n) {
            return Err("a chain shorter than its file".into());
        }
        Ok(out)
    }

    /// The bytes of the clusters `chain`.
    fn bytes(&self, chain: &[u32]) -> Vec<u8> {
        chain.iter().flat_map(|&c| self.d[self.at(c)].iter().copied()).collect()
    }
}

/// Free ranges of FAT12/16/32 volume `d`.
pub(super) fn free(d: &[u8]) -> Result<Vec<Range<usize>>, String> {
    let mut v = Vol::new(d)?;
    let mut tails: Vec<Range<usize>> = Vec::new();
    // (the directories to read: their bytes)
    let mut todo: Vec<Vec<u8>> = Vec::new();
    match v.root.clone() {
        Ok(r) => todo.push(d[r].to_vec()),
        Err(c) => {
            let chain = v.follow(c, None)?;
            todo.push(v.bytes(&chain));
        }
    }
    while let Some(dir) = todo.pop() {
        for e in dir.as_chunks::<32>().0 {
            match e[0] {
                0 => break,
                0xe5 => continue,
                _ => {}
            }
            let attr = e[11];
            // (a long name's entry, the volume's label, and the entries of . and ..)
            if attr & 0x3f == 0x0f || attr & 0x08 != 0 {
                continue;
            }
            let dots = e[..11].starts_with(b".") && e[1..11].iter().all(|&b| b == b' ' || b == b'.');
            if attr & 0x10 != 0 && dots {
                continue;
            }
            let high = if v.width == Width::Fat32 { le16(e, 20) << 16 } else { 0 };
            let first = high | le16(e, 26);
            if attr & 0x10 != 0 {
                if first < 2 {
                    return Err("a directory of no cluster".into());
                }
                let chain = v.follow(first, None)?;
                todo.push(v.bytes(&chain));
                continue;
            }
            let size = le32(e, 28) as usize;
            if size == 0 {
                if first != 0 {
                    return Err("an empty file with a cluster".into());
                }
                continue;
            }
            let n = size.div_ceil(v.cluster);
            let chain = v.follow(first, Some(n))?;
            let used = size - (n - 1) * v.cluster;
            let last = v.at(chain[n - 1]);
            tails.push(last.start + used..last.end);
        }
    }
    let mut out = tails;
    for c in 2..v.count + 2 {
        if v.entry(c) == 0 {
            debug_assert!(!v.owned[c as usize]);
            out.push(v.at(c));
        }
    }
    Ok(merged(out))
}

/// Free ranges of exFAT volume `d`.
pub(super) fn free_exfat(d: &[u8]) -> Result<Vec<Range<usize>>, String> {
    let bad = || format!("{NEVER} boot sector");
    let b = d.get(..512).ok_or_else(bad)?;
    if &b[3..11] != b"EXFAT   " || b[11..64].iter().any(|&x| x != 0) || b[510..512] != [0x55, 0xaa] {
        return Err(bad());
    }
    let (sector, per) = (b[108] as u32, b[109] as u32);
    if !(9..=12).contains(&sector) || sector + per > 25 || b[110] != 1 {
        return Err(if b[110] == 2 { "two FATs (TexFAT)".into() } else { bad() });
    }
    if le16(b, 106) & 2 != 0 {
        return Err("a volume not unmounted cleanly".into());
    }
    let (fat_off, fat_len, heap_off) = (le32(b, 80) as u64, le32(b, 84) as u64, le32(b, 88) as u64);
    let (count, root) = (le32(b, 92), le32(b, 96));
    let length = le64(b, 72);
    if fat_off < 24 || fat_len < 1 || fat_off + fat_len > heap_off || !(1..=0xFFFF_FFF5).contains(&count) {
        return Err(bad());
    }
    if !(2..=count + 1).contains(&root) || (fat_len << sector) / 4 < count as u64 + 2 {
        return Err(bad());
    }
    let cluster = 1usize << (sector + per);
    let heap = usize::try_from(heap_off << sector).map_err(|_| bad())?;
    let end = (count as usize).checked_mul(cluster).and_then(|n| n.checked_add(heap)).ok_or_else(bad)?;
    if end > d.len() || (end as u64) > length << sector {
        return Err("volume shorter than its boot sector tells".into());
    }
    let fat_at = (fat_off << sector) as usize;
    let fat = &d[fat_at..fat_at + ((fat_len << sector) as usize)];
    let x = Ex { fat, cluster, heap, count };
    let mut owned = vec![false; count as usize + 2];
    // (what the root tells of the bitmap, and the first clusters of the directories read)
    let mut bitmap: Option<(u32, u64)> = None;
    let mut tails: Vec<Range<usize>> = Vec::new();
    let mut todo: Vec<Vec<u32>> = vec![x.follow(&mut owned, root, None)?];
    let mut is_root = true;
    while let Some(chain) = todo.pop() {
        let dir: Vec<u8> = chain.iter().flat_map(|&c| d[x.at(c)].iter().copied()).collect();
        let entries = dir.as_chunks::<32>().0;
        let mut i = 0;
        while i < entries.len() {
            let e = &entries[i];
            i += 1;
            if e[0] == 0 {
                break;
            }
            if e[0] & 0x80 == 0 {
                continue;
            }
            match e[0] {
                0x83 | 0xa0 => {}
                0x81 | 0x82 => {
                    let (first, len) = (le32(e, 20), le64(e, 24));
                    if e[0] == 0x81 {
                        if !is_root || bitmap.is_some() || e[1] & 1 != 0 {
                            return Err("an unexpected allocation bitmap".into());
                        }
                        bitmap = Some((first, len));
                    }
                    // (as Windows and others write them: in the FAT's chain, or in a run)
                    let nofat = first >= 2 && first <= count + 1 && x.entry(first) == 0;
                    x.stream(&mut owned, first, nofat, len)?;
                }
                0x85 => {
                    let secondary = e[1] as usize;
                    let set = entries.get(i..i + secondary).filter(|_| (2..=18).contains(&secondary));
                    let set = set.ok_or("an entry set cut short")?;
                    i += secondary;
                    let s = &set[0];
                    if s[0] != 0xc0 || set[1..].iter().any(|n| n[0] != 0xc1) {
                        return Err("an entry set of no stream and names".into());
                    }
                    let (flags, valid, first, len) = (s[1], le64(s, 8), le32(s, 20), le64(s, 24));
                    if valid > len || flags & 1 == 0 && len != 0 {
                        return Err("a file's lengths invalid".into());
                    }
                    let chain = x.stream(&mut owned, first, flags & 2 != 0, len)?;
                    if le16(e, 4) & 0x10 != 0 {
                        if !chain.is_empty() {
                            todo.push(chain);
                        }
                    } else if let Some(&last) = chain.last() {
                        let used = (len - (chain.len() as u64 - 1) * cluster as u64) as usize;
                        let r = x.at(last);
                        tails.push(r.start + used..r.end);
                    }
                }
                _ => return Err("an unknown directory entry".into()),
            }
        }
        is_root = false;
    }
    let (first, len) = bitmap.ok_or("no allocation bitmap")?;
    if len < (count as u64).div_ceil(8) {
        return Err("an allocation bitmap too short".into());
    }
    let nofat = x.entry(first) == 0;
    let chain = x.stream_peek(first, nofat, len)?;
    let bits: Vec<u8> = chain.iter().flat_map(|&c| d[x.at(c)].iter().copied()).collect();
    let set = |c: u32| bits[(c as usize - 2) / 8] >> ((c as usize - 2) % 8) & 1 != 0;
    let mut out = tails;
    for c in 2..count + 2 {
        match (set(c), owned[c as usize]) {
            (false, true) => return Err("a cluster of a file marked free in the bitmap".into()),
            (false, false) => out.push(x.at(c)),
            _ => {}
        }
    }
    Ok(merged(out))
}

struct Ex<'a> {
    fat: &'a [u8],
    cluster: usize,
    heap: usize,
    count: u32,
}

impl Ex<'_> {
    fn entry(&self, c: u32) -> u32 {
        le32(self.fat, c as usize * 4)
    }

    fn at(&self, c: u32) -> Range<usize> {
        let s = self.heap + (c as usize - 2) * self.cluster;
        s..s + self.cluster
    }

    /// The chain from `first` (to its end), claimed.
    fn follow(&self, owned: &mut [bool], first: u32, n: Option<usize>) -> Result<Vec<u32>, String> {
        let mut out = Vec::new();
        let mut c = first;
        loop {
            if !(2..=self.count + 1).contains(&c) {
                return Err("a chain past the clusters".into());
            }
            if std::mem::replace(&mut owned[c as usize], true) {
                return Err("a cluster reached twice (a chain looping or crossing)".into());
            }
            out.push(c);
            if n.is_some_and(|n| out.len() > n) {
                return Err("a chain longer than its file".into());
            }
            let e = self.entry(c);
            if e >= 0xffff_fff8 {
                break;
            }
            if e == 0xffff_fff7 || e == 0 {
                return Err("a chain through a bad or free cluster".into());
            }
            c = e;
        }
        if n.is_some_and(|n| out.len() != n) {
            return Err("a chain shorter than its file".into());
        }
        Ok(out)
    }

    /// The clusters of a stream of `len` bytes from `first`, claimed: a run (NoFatChain) or the
    /// FAT's chain; none for no bytes (which have no cluster).
    fn stream(&self, owned: &mut [bool], first: u32, nofat: bool, len: u64) -> Result<Vec<u32>, String> {
        if len == 0 {
            return if first == 0 { Ok(Vec::new()) } else { Err("an empty stream with a cluster".into()) };
        }
        let n = usize::try_from(len.div_ceil(self.cluster as u64)).map_err(|_| "a stream too long")?;
        if n > self.count as usize {
            return Err("a stream longer than the clusters".into());
        }
        if !nofat {
            return self.follow(owned, first, Some(n));
        }
        let run = self.run(first, n)?;
        for &c in &run {
            if std::mem::replace(&mut owned[c as usize], true) {
                return Err("a cluster reached twice (a chain looping or crossing)".into());
            }
        }
        Ok(run)
    }

    /// The clusters of a stream again, unclaimed (the bitmap's: it is claimed already).
    fn stream_peek(&self, first: u32, nofat: bool, len: u64) -> Result<Vec<u32>, String> {
        let n = usize::try_from(len.div_ceil(self.cluster as u64)).map_err(|_| "a stream too long")?;
        if nofat {
            return self.run(first, n);
        }
        let mut seen = vec![false; self.count as usize + 2];
        self.follow(&mut seen, first, Some(n))
    }

    fn run(&self, first: u32, n: usize) -> Result<Vec<u32>, String> {
        let last = (first as u64).checked_add(n as u64 - 1).filter(|&l| first >= 2 && l <= self.count as u64 + 1);
        let last = last.ok_or("a run past the clusters")? as u32;
        Ok((first..=last).collect())
    }
}
