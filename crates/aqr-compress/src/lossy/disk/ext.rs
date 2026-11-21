//! The free bytes of a volume of ext2/3/4 (<https://www.kernel.org/doc/html/latest/filesystems/ext4/>),
//! written from the on-disk format alone (not by aqr-ext, so that this and aqr-disk's listing are
//! two reads): the blocks whose bit in their group's block bitmap is 0.
//!
//! Told: only groups whose bitmap is there to read (a group with `BLOCK_UNINIT` has none: skipped),
//! within the blocks count, never block 0. Refused: features this does not know (bigalloc,
//! sparse_super2, MMP, an external journal, compression, ...), a journal that is to be
//! replayed (`needs_recovery`, or a journal whose `s_start` is not 0), a file system with errors,
//! a group descriptor of a wrong checksum, a bitmap or an inode table out of the volume, and a
//! free block that is one of the structures (a bitmap, an inode table, the superblock or the
//! group descriptors, their reserved blocks, the journal's blocks) which a bitmap must not free.

use std::ops::Range;

fn le16(d: &[u8], p: usize) -> u64 {
    u16::from_le_bytes([d[p], d[p + 1]]) as u64
}

fn le32(d: &[u8], p: usize) -> u64 {
    u32::from_le_bytes([d[p], d[p + 1], d[p + 2], d[p + 3]]) as u64
}

/// CRC-16 as ext's `crc16` (poly 0xA001 reflected), seeded with `c`.
fn crc16(mut c: u16, d: &[u8]) -> u16 {
    for &b in d {
        c ^= b as u16;
        for _ in 0..8 {
            c = if c & 1 != 0 { c >> 1 ^ 0xA001 } else { c >> 1 };
        }
    }
    c
}

/// CRC-32C (Castagnoli, reflected, no final inversion: as ext's `crc32c`), seeded with `c`.
fn crc32c(mut c: u32, d: &[u8]) -> u32 {
    for &b in d {
        c ^= b as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 { c >> 1 ^ 0x82F6_3B78 } else { c >> 1 };
        }
    }
    c
}

const INCOMPAT_RECOVER: u64 = 0x4;
const INCOMPAT_OK: u64 = 0x2 /* filetype */ | 0x10 /* meta_bg */ | 0x40 /* extents */ | 0x80 /* 64bit */ | 0x200 /* flex_bg */
    | 0x400 /* ea_inode */ | 0x2000 /* csum_seed */ | 0x4000 /* largedir */ | 0x8000 /* inline_data */
    | 0x10000 /* encrypt */ | 0x20000 /* casefold */;
const COMPAT_SPARSE_SUPER2: u64 = 0x200;
const COMPAT_HAS_JOURNAL: u64 = 0x4;
const RO_SPARSE_SUPER: u64 = 0x1;
const RO_BIGALLOC: u64 = 0x200;
const RO_GDT_CSUM: u64 = 0x10;
const RO_METADATA_CSUM: u64 = 0x400;
const BLOCK_UNINIT: u64 = 0x2;

struct Fs<'a> {
    d: &'a [u8],
    bs: usize,
    blocks: u64,
    first: u64,
    bpg: u64,
    ipg: u64,
    /// The first group of meta_bg's layout (past the last group without meta_bg).
    meta: u64,
    isize: u64,
    groups: u64,
    desc: usize,
    resv: u64,
    sparse: bool,
}

impl Fs<'_> {
    fn block(&self, n: u64) -> Option<&[u8]> {
        if n >= self.blocks {
            return None;
        }
        self.d.get(n as usize * self.bs..(n as usize + 1) * self.bs)
    }
    /// The start of the group `g`'s descriptor.
    fn desc_at(&self, g: u64) -> usize {
        let dpb = (self.bs / self.desc) as u64;
        if g < self.meta {
            return (self.first + 1) as usize * self.bs + g as usize * self.desc;
        }
        let m = g / dpb * dpb;
        (self.first + m * self.bpg + self.backup(m) as u64) as usize * self.bs + (g - m) as usize * self.desc
    }
    fn desc(&self, g: u64) -> &[u8] {
        let at = self.desc_at(g);
        &self.d[at..at + self.desc]
    }
    /// The blocks of the group `g` that its copy of the superblock and descriptors takes.
    fn backup_blocks(&self, g: u64) -> u64 {
        let dpb = (self.bs / self.desc) as u64;
        let old = self.groups.div_ceil(dpb).min(self.meta / dpb);
        let meta = g >= self.meta && matches!(g % dpb, 0 | 1) || g >= self.meta && g % dpb == dpb - 1;
        self.backup(g) as u64 * (1 + old + self.resv) + meta as u64
    }
    /// Whether group `g` has a backup of the superblock and the descriptors.
    fn backup(&self, g: u64) -> bool {
        if g == 0 || !self.sparse || g == 1 {
            return true;
        }
        [3u64, 5, 7].iter().any(|&p| {
            let mut x = p;
            while x < g {
                x *= p;
            }
            x == g
        })
    }
}

pub(super) fn free(d: &[u8]) -> Result<Vec<Range<usize>>, String> {
    if d.len() < 2048 || le16(d, 1024 + 56) != 0xEF53 {
        return Err("no superblock".into());
    }
    let sb = &d[1024..2048];
    let compat = le32(sb, 92);
    let incompat = le32(sb, 96);
    let ro = le32(sb, 100);
    if incompat & !INCOMPAT_OK & !INCOMPAT_RECOVER != 0 {
        return Err(format!("incompatible features {:#x} not known", incompat & !INCOMPAT_OK & !INCOMPAT_RECOVER));
    }
    if incompat & INCOMPAT_RECOVER != 0 {
        return Err("the journal needs recovery".into());
    }
    if ro & RO_BIGALLOC != 0 || compat & COMPAT_SPARSE_SUPER2 != 0 {
        return Err("bigalloc or sparse_super2".into());
    }
    if le16(sb, 58) != 1 {
        return Err("not cleanly unmounted, or with errors".into());
    }
    let log = le32(sb, 24);
    if log > 6 {
        return Err("block size out of range".into());
    }
    let bs = 1024usize << log;
    let wide = incompat & 0x80 != 0;
    let mut blocks = le32(sb, 4);
    let mut desc = 32usize;
    if wide {
        blocks |= le32(sb, 0x150) << 32;
        desc = le16(sb, 254) as usize;
        if desc < 64 || desc > bs || !desc.is_power_of_two() {
            return Err("group descriptor size out of range".into());
        }
    }
    let (first, bpg, ipg, isize) = (le32(sb, 20), le32(sb, 32), le32(sb, 40), le16(sb, 88));
    if bpg == 0 || bpg > bs as u64 * 8 || ipg == 0 || isize < 128 || isize > bs as u64 || first > 1 {
        return Err("geometry out of range".into());
    }
    if (first == 1) != (bs == 1024) {
        return Err("first data block does not fit the block size".into());
    }
    if blocks <= first || blocks * bs as u64 > d.len() as u64 {
        return Err("blocks count out of range".into());
    }
    let groups = (blocks - first).div_ceil(bpg);
    let dpb = (bs / if wide { desc } else { 32 }) as u64;
    let meta = if incompat & 0x10 != 0 { le32(sb, 0x104).saturating_mul(dpb) } else { u64::MAX };
    let fs = Fs {
        d,
        bs,
        blocks,
        first,
        bpg,
        ipg,
        meta,
        isize,
        groups,
        desc,
        resv: le16(sb, 206),
        sparse: ro & RO_SPARSE_SUPER != 0,
    };
    if (0..groups).any(|g| fs.desc_at(g) + desc > d.len()) {
        return Err("descriptors out of the volume".into());
    }
    // the checksum of the descriptors
    let uuid = &sb[104..120];
    // (ext's crc32c does not invert: the seed is crc32c(~0, uuid), or the superblock's own)
    let seed = if incompat & 0x2000 != 0 { le32(sb, 0x270) as u32 } else { crc32c(!0, uuid) };
    let itable_blocks = (ipg * isize).div_ceil(bs as u64);
    // blocks that must stay allocated
    let mut must: Vec<Range<u64>> = Vec::new();
    must.push(0..first.max(1));
    let mut bitmaps = Vec::new();
    for g in 0..groups {
        let e = fs.desc(g);
        let hi = |p: usize| if wide { le32(e, p) << 32 } else { 0 };
        let (bb, ib, it) = (le32(e, 0) | hi(0x20), le32(e, 4) | hi(0x24), le32(e, 8) | hi(0x28));
        let flags = le16(e, 0x12);
        if ro & RO_METADATA_CSUM != 0 {
            let mut c = crc32c(seed, &(g as u32).to_le_bytes());
            c = crc32c(c, &e[..0x1E]);
            c = crc32c(c, &[0, 0]);
            c = crc32c(c, &e[0x20..]);
            if c as u16 as u64 != le16(e, 0x1E) {
                return Err(format!("group {g}'s descriptor checksum is wrong"));
            }
        } else if ro & RO_GDT_CSUM != 0 {
            let mut c = crc16(!0, uuid);
            c = crc16(c, &(g as u32).to_le_bytes());
            c = crc16(c, &e[..0x1E]);
            if desc > 0x20 {
                c = crc16(c, &e[0x20..]);
            }
            if c as u64 != le16(e, 0x1E) {
                return Err(format!("group {g}'s descriptor checksum is wrong"));
            }
        }
        if bb == 0 || ib == 0 || it == 0 || bb >= blocks || ib >= blocks || it + itable_blocks > blocks {
            return Err(format!("group {g}'s bitmaps or inode table out of the volume"));
        }
        must.push(bb..bb + 1);
        must.push(ib..ib + 1);
        must.push(it..it + itable_blocks);
        let at = first + g * bpg;
        must.push(at..(at + fs.backup_blocks(g)).min(blocks));
        if flags & BLOCK_UNINIT == 0 {
            bitmaps.push((g, bb));
        }
    }
    // the journal's blocks
    if compat & COMPAT_HAS_JOURNAL != 0 {
        let inum = le32(sb, 224);
        if inum == 0 {
            return Err("a journal of no inode".into());
        }
        let js = journal(&fs, inum, &mut must)?;
        // (a journal holding transactions is replayed whatever the flag says)
        if let Some(b) = js
            && le32(fs.block(b).ok_or("journal out of the volume")?, 0x1C) != 0
        {
            return Err("the journal holds transactions".into());
        }
    }
    must.sort_unstable_by_key(|r| r.start);
    // the free blocks
    let mut out: Vec<Range<usize>> = Vec::new();
    for (g, bb) in bitmaps {
        let bm = fs.block(bb).ok_or("bitmap out of the volume")?;
        let start = first + g * bpg;
        let end = (start + bpg).min(blocks);
        for b in start..end {
            let i = (b - start) as usize;
            if bm[i / 8] >> (i % 8) & 1 != 0 {
                continue;
            }
            let k = must.partition_point(|r| r.end <= b);
            if must.get(k).is_some_and(|r| r.start <= b) || b == 0 {
                return Err(format!("block {b} is free but is of a structure"));
            }
            let r = b as usize * bs..(b as usize + 1) * bs;
            match out.last_mut() {
                Some(l) if l.end == r.start => l.end = r.end,
                _ => out.push(r),
            }
        }
    }
    Ok(out)
}

/// The blocks of the journal inode `inum` pushed on `must`; its first block.
fn journal(fs: &Fs, inum: u64, must: &mut Vec<Range<u64>>) -> Result<Option<u64>, String> {
    let bad = |s: &str| Err(format!("journal inode: {s}"));
    if inum > fs.ipg * fs.groups {
        return bad("out of range");
    }
    let g = (inum - 1) / fs.ipg;
    let e = fs.desc(g);
    let it = le32(e, 8) | if fs.desc > 32 { le32(e, 0x28) << 32 } else { 0 };
    let at = it as usize * fs.bs + ((inum - 1) % fs.ipg * fs.isize) as usize;
    let Some(ino) = fs.d.get(at..at + 128) else { return bad("out of the volume") };
    let flags = le32(ino, 32);
    let mut runs: Vec<(u64, u64)> = Vec::new();
    let map = &ino[40..100];
    if flags & 0x8_0000 != 0 {
        extents(fs, map, 0, &mut runs)?;
    } else {
        let ptr = |b: &[u8], i: usize| le32(b, i * 4);
        let per = fs.bs as u64 / 4;
        for i in 0..12 {
            if ptr(map, i) != 0 {
                runs.push((ptr(map, i), 1));
            }
        }
        fn walk(fs: &Fs, b: u64, level: u32, per: u64, runs: &mut Vec<(u64, u64)>) -> Result<(), String> {
            if b == 0 {
                return Ok(());
            }
            let Some(blk) = fs.block(b) else { return Err("journal inode: block out of the volume".into()) };
            runs.push((b, 1));
            for i in 0..per as usize {
                let p = le32(blk, i * 4);
                if p == 0 {
                    continue;
                }
                if level == 1 {
                    runs.push((p, 1));
                } else {
                    walk(fs, p, level - 1, per, runs)?;
                }
            }
            Ok(())
        }
        for (i, lv) in [(12, 1), (13, 2), (14, 3)] {
            walk(fs, ptr(map, i), lv, per, &mut runs)?;
        }
    }
    let first = runs.iter().map(|r| r.0).min();
    for (b, n) in runs {
        if b + n > fs.blocks {
            return bad("blocks out of the volume");
        }
        must.push(b..b + n);
    }
    Ok(first)
}

/// The extents of the node `n` (header and entries), the blocks of the tree's nodes too.
fn extents(fs: &Fs, n: &[u8], depth: u32, runs: &mut Vec<(u64, u64)>) -> Result<(), String> {
    if n.len() < 12 || le16(n, 0) != 0xF30A || depth > 5 {
        return Err("journal inode: extent tree damaged".into());
    }
    let (entries, level) = (le16(n, 2) as usize, le16(n, 6));
    if 12 + entries * 12 > n.len() {
        return Err("journal inode: extent tree damaged".into());
    }
    for i in 0..entries {
        let e = &n[12 + i * 12..24 + i * 12];
        if level == 0 {
            let len = le16(e, 4);
            let len = if len > 32768 { len - 32768 } else { len };
            runs.push(((le16(e, 6) << 32) | le32(e, 8), len));
        } else {
            let b = le32(e, 4) | le16(e, 8) << 32;
            let Some(blk) = fs.block(b) else { return Err("journal inode: tree node out of the volume".into()) };
            runs.push((b, 1));
            extents(fs, blk, depth + 1, runs)?;
        }
    }
    Ok(())
}
