//! ext2, ext3 and ext4 volumes read into their files: a file system of aqr-disk ([`FS`]), which
//! takes the image apart into the contents of the files and the rest (the superblocks, the group
//! descriptors, bitmaps and inode tables, directories, the journal, free blocks).
//!
//! What it reads (from the kernel's `Documentation/filesystems/ext4/`, the documentation alone; no
//! implementation's code read): the superblock; the group descriptors (of 32 bytes, or of
//! `s_desc_size` with `64bit`; where `meta_bg` puts them too); the inodes; the directories from the
//! root, their entries read linearly (an htree's index blocks read as the empty entries they are
//! made to look like); a regular file's blocks by its extent tree (ext4) or its block map (direct,
//! indirect, double and triple indirect blocks: ext2, ext3). A file's contents are its blocks up to
//! its size, holes and unwritten extents left out.
//!
//! Left in the rest: inline data (in the inode), encrypted files, symbolic links, directories,
//! extended attributes, the journal, and what does not read.

use std::borrow::Cow;
use std::collections::HashSet;
use std::ops::Range;

pub use aqr_disk::{File, Image};

/// ext2, ext3 and ext4 as a file system of aqr-disk.
pub const FS: aqr_disk::FileSystem = aqr_disk::FileSystem { name: "ext", starts, files };

/// Deepest directory read, from the root, and deepest extent tree.
const DEPTH: usize = 256;
const TREE: usize = 8;

const ROOT: u32 = 2;

fn le16(b: &[u8], p: usize) -> u16 {
    u16::from_le_bytes([b[p], b[p + 1]])
}

fn le32(b: &[u8], p: usize) -> u32 {
    u32::from_le_bytes(b[p..p + 4].try_into().unwrap())
}

/// What the superblock tells of a volume.
struct Volume {
    block: u64,
    blocks: u64,
    inodes: u32,
    per_group: u32,
    inode_size: usize,
    /// Where each group's inode table starts (a block).
    tables: Vec<u64>,
    filetype: bool,
    /// Blocks still to be followed in all: of a volume whose files' blocks meet or loop (as
    /// damaged or made ones do), the time stays that of the blocks.
    budget: std::cell::Cell<u64>,
}

/// The superblock of `d` (at byte 1024), where it is one: the magic, sizes the format allows; and
/// the group descriptors' inode tables.
fn volume(d: &dyn Image) -> Option<Volume> {
    let s = d.get(1024..2048)?;
    let s = &s[..];
    if le16(s, 56) != 0xef53 {
        return None;
    }
    let log = le32(s, 24);
    if log > 6 {
        return None;
    }
    let block = 1024u64 << log;
    let incompat = le32(s, 96);
    let wide = incompat & 0x80 != 0;
    let blocks = le32(s, 4) as u64 | if wide { (le32(s, 0x150) as u64) << 32 } else { 0 };
    let (inodes, per_block_group, per_group) = (le32(s, 0), le32(s, 32), le32(s, 40));
    let first = le32(s, 20) as u64;
    let inode_size = match le32(s, 76) {
        0 => 128,
        _ => le16(s, 88) as usize,
    };
    let desc = match wide {
        true => le16(s, 0xfe) as usize,
        false => 32,
    };
    if per_group == 0
        || per_block_group == 0
        || blocks <= first
        || !(128..=block as usize).contains(&inode_size)
        || !inode_size.is_power_of_two()
        || !(32..=1024).contains(&desc)
        || !desc.is_power_of_two()
        || first > 1
    {
        return None;
    }
    let groups = (blocks - first).div_ceil(per_block_group as u64);
    if groups.checked_mul(per_group as u64)? < inodes as u64 || groups > d.len() as u64 / desc as u64 + 1 {
        return None;
    }
    // the descriptors: in the blocks after the superblock's, but where meta_bg puts those from
    // s_first_meta_bg on, each block of them in the first group of its own groups (after the
    // superblock's backup, where that group has one: of groups 0, 1, and powers of 3, 5 and 7,
    // with sparse_super)
    let per_block = (block as usize / desc) as u64;
    let meta = incompat & 0x10 != 0;
    let first_meta = le32(s, 0x104) as u64;
    let sparse = le32(s, 100) & 1 != 0;
    let backup = |g: u64| !sparse || g <= 1 || [3u64, 5, 7].iter().any(|&p| (0..40).map(|k| p.pow(k)).any(|x| x == g));
    let mut tables = Vec::with_capacity(groups as usize);
    for g in 0..groups {
        let b = g / per_block;
        let at = match meta && b >= first_meta {
            true => first + b * per_block * per_block_group as u64 + backup(b * per_block) as u64,
            false => first + 1 + b,
        };
        let p = at.checked_mul(block)?.checked_add((g % per_block) * desc as u64)?;
        let Some(e) = usize::try_from(p).ok().and_then(|p| d.get(p..p + desc)) else { break };
        let e = &e[..];
        let hi = if wide && desc >= 64 { (le32(e, 0x28) as u64) << 32 } else { 0 };
        tables.push(le32(e, 8) as u64 | hi);
    }
    let budget = std::cell::Cell::new(2 * blocks.min(d.len() as u64 / block + 1) + 1024);
    Some(Volume { block, blocks, inodes, per_group, inode_size, tables, filetype: incompat & 0x2 != 0, budget })
}

/// Where the inode table of each group of ext volume `d` starts (a block), as its descriptors
/// tell: for tools and tests.
pub fn inode_tables(d: &dyn Image) -> Vec<u64> {
    volume(d).map(|v| v.tables).unwrap_or_default()
}

/// Whether `d` is an ext2, ext3 or ext4 volume ([`volume`]).
pub fn starts(d: &dyn Image) -> bool {
    volume(d).is_some()
}

impl Volume {
    /// The bytes of block `n` of `d`, where it is in both.
    fn block<'a>(&self, d: &'a dyn Image, n: u64) -> Option<Cow<'a, [u8]>> {
        if n >= self.blocks {
            return None;
        }
        let at = usize::try_from(n.checked_mul(self.block)?).ok()?;
        d.get(at..at.checked_add(self.block as usize)?)
    }

    /// Inode `n` of `d`.
    fn inode<'a>(&self, d: &'a dyn Image, n: u32) -> Option<Cow<'a, [u8]>> {
        if n == 0 || n > self.inodes {
            return None;
        }
        let (g, i) = ((n - 1) / self.per_group, (n - 1) % self.per_group);
        let at =
            self.tables.get(g as usize)?.checked_mul(self.block)?.checked_add(i as u64 * self.inode_size as u64)?;
        let at = usize::try_from(at).ok()?;
        d.get(at..at.checked_add(self.inode_size)?)
    }

    /// The blocks of inode `i`'s data, in order of the file: runs of a logical block, a physical
    /// block and a count (unwritten extents and holes not among them), those of logical blocks
    /// before `most`.
    fn runs(&self, d: &dyn Image, i: &[u8], most: u64) -> Vec<(u64, u64, u64)> {
        let mut out = Vec::new();
        let flags = le32(i, 32);
        if flags & 0x80000 != 0 {
            self.extents(d, &i[40..100], most, 0, &mut out);
        } else {
            self.mapped(d, i, most, &mut out);
        }
        out.sort_by_key(|r| r.0);
        out
    }

    /// The extents of the node of an extent tree `n` (its header and entries), of depth `depth`.
    fn extents(&self, d: &dyn Image, n: &[u8], most: u64, depth: usize, out: &mut Vec<(u64, u64, u64)>) {
        if n.len() < 12 || le16(n, 0) != 0xf30a || depth > TREE {
            return;
        }
        let (entries, level) = (le16(n, 2) as usize, le16(n, 6));
        for k in 0..entries.min((n.len() - 12) / 12) {
            let e = &n[12 + 12 * k..][..12];
            let logical = le32(e, 0) as u64;
            if logical >= most || self.budget.get() == 0 {
                break;
            }
            if level == 0 {
                let len = le16(e, 4) as u64;
                let physical = (le16(e, 6) as u64) << 32 | le32(e, 8) as u64;
                // (more than 32768: unwritten, read as zeros)
                if len == 0 || len > 32768 {
                    continue;
                }
                let len = len.min(self.budget.get());
                self.budget.set(self.budget.get() - len);
                out.push((logical, physical, len));
            } else {
                let child = (le16(e, 8) as u64) << 32 | le32(e, 4) as u64;
                self.budget.set(self.budget.get() - 1);
                if let Some(b) = self.block(d, child) {
                    self.extents(d, &b, most, depth + 1, out);
                }
            }
        }
    }

    /// The blocks of block map `i` (12 direct, then an indirect, a double and a triple indirect
    /// block), 0 a hole.
    fn mapped(&self, d: &dyn Image, i: &[u8], most: u64, out: &mut Vec<(u64, u64, u64)>) {
        let per = self.block / 4;
        let mut push = |logical: u64, physical: u64| match out.last_mut() {
            Some(r) if r.0 + r.2 == logical && r.1 + r.2 == physical => r.2 += 1,
            _ => out.push((logical, physical, 1)),
        };
        for k in 0..12u64 {
            let p = le32(i, 40 + 4 * k as usize) as u64;
            if k < most && p != 0 {
                push(k, p);
            }
        }
        // (an indirect block of `level` levels from logical block `base`)
        fn walk(v: &Volume, d: &dyn Image, b: u64, level: u32, base: u64, most: u64, push: &mut dyn FnMut(u64, u64)) {
            let per = v.block / 4;
            let Some(blk) = v.block(d, b) else { return };
            let span = per.pow(level - 1);
            for k in 0..per {
                let logical = base + k * span;
                if logical >= most || v.budget.get() == 0 {
                    return;
                }
                let p = le32(&blk, 4 * k as usize) as u64;
                if p == 0 {
                    continue;
                }
                v.budget.set(v.budget.get() - 1);
                match level {
                    1 => push(logical, p),
                    _ => walk(v, d, p, level - 1, logical, most, push),
                }
            }
        }
        let mut base = 12u64;
        for level in 1..=3u32 {
            let p = le32(i, 40 + 4 * (11 + level as usize)) as u64;
            if p != 0 && base < most {
                walk(self, d, p, level, base, most, &mut push);
            }
            base = base.saturating_add(per.saturating_pow(level));
        }
    }

    /// The byte ranges of `runs` up to `len` bytes (holes counted, not taken).
    fn ranges(&self, runs: &[(u64, u64, u64)], len: u64) -> Vec<Range<usize>> {
        let mut out: Vec<Range<usize>> = Vec::new();
        for &(logical, physical, n) in runs {
            let from = logical.saturating_mul(self.block);
            if from >= len {
                break;
            }
            let bytes = n.saturating_mul(self.block).min(len - from);
            let Some(start) = physical.checked_mul(self.block).and_then(|s| usize::try_from(s).ok()) else {
                continue;
            };
            let end = start.saturating_add(bytes as usize);
            match out.last_mut() {
                Some(r) if r.end == start => r.end = end,
                _ => out.push(start..end),
            }
        }
        out
    }
}

/// The size of inode `i`.
fn size(i: &[u8]) -> u64 {
    le32(i, 4) as u64 | (le32(i, 108) as u64) << 32
}

/// The files of ext volume `d`.
pub fn files(d: &dyn Image) -> Vec<File> {
    let Some(v) = volume(d) else { return Vec::new() };
    let mut out = Vec::new();
    let mut seen = HashSet::from([ROOT]);
    let mut taken = HashSet::new();
    let mut todo: Vec<(Vec<u8>, u32, usize)> = vec![(Vec::new(), ROOT, 0)];
    // (bytes of directories read in all: at most the volume's)
    let mut bytes = d.len() as u64;
    while let Some((path, n, depth)) = todo.pop() {
        let Some(dir) = v.inode(d, n) else { continue };
        let dir = &dir[..];
        if le32(dir, 32) & 0x1000_0800 != 0 {
            // (inline or encrypted: in the rest)
            continue;
        }
        let len = size(dir).min(bytes);
        bytes -= len;
        let runs = v.runs(d, dir, len.div_ceil(v.block));
        for r in v.ranges(&runs, len) {
            let blk = d.cut(r);
            // the entries of each block: the inode, the record's length, the name's
            for b in blk.chunks(v.block as usize) {
                let mut at = 0;
                while at + 8 <= b.len() {
                    let (ino, rec) = (le32(b, at), le16(b, at + 4) as usize);
                    let nlen = b[at + 6] as usize;
                    if rec < 8 || at + rec > b.len() {
                        break;
                    }
                    let kind = if v.filetype { b[at + 7] } else { 0 };
                    let name = &b[at + 8..(at + 8 + nlen).min(at + rec)];
                    at += rec;
                    if ino == 0 || name == b"." || name == b".." || name.is_empty() {
                        continue;
                    }
                    let Some(i) = v.inode(d, ino) else { continue };
                    let i = &i[..];
                    let mode = le16(i, 0) & 0xf000;
                    let full = match path.is_empty() {
                        true => name.to_vec(),
                        false => [&path[..], b"/", name].concat(),
                    };
                    if mode == 0x4000 && kind != 1 {
                        if depth < DEPTH && seen.insert(ino) {
                            todo.push((full, ino, depth + 1));
                        }
                        continue;
                    }
                    // regular files, each once (hard links), but inline and encrypted ones
                    if mode != 0x8000 || le32(i, 32) & 0x1000_0800 != 0 || !taken.insert(ino) {
                        continue;
                    }
                    let len = size(i);
                    let runs = v.runs(d, i, len.div_ceil(v.block));
                    out.push(File { name: full, extents: v.ranges(&runs, len), rank: 0 });
                }
            }
        }
    }
    out
}
