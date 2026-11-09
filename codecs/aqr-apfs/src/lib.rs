//! APFS containers read into their files: a file system of aqr-disk ([`FS`]), which takes the image
//! apart into the contents of the files and the rest (the superblocks, the object maps and file-system
//! trees, free blocks, the reaper and the space manager).
//!
//! What it reads (from Apple's *Apple File System Reference*, the document alone; no
//! implementation's code read): the container superblock (the newest valid one of the checkpoint
//! descriptor area, else block 0); the container's object map; each volume's superblock, object map
//! and file-system tree (B-trees: their nodes, fixed or variable key and value sizes); in the tree
//! the directory records, inodes and file extents; the directories from the root, each once, at
//! most 256 deep. A regular file's contents are the extents of its data stream (its inode's
//! `private_id`, so a clone's are those of its original) up to its size, holes left out.
//!
//! Left in the rest: decmpfs-compressed files (their data is in an extended attribute or the
//! resource fork), symbolic links, directories, extended attributes, snapshots, encrypted and
//! sealed volumes, everything of the container but the files' blocks.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::ops::Range;

pub use aqr_disk::{File, Image};

/// APFS as a file system of aqr-disk.
pub const FS: aqr_disk::FileSystem = aqr_disk::FileSystem { name: "apfs", starts, files };

/// Deepest directory read, from the root, and deepest B-tree.
const DEPTH: usize = 256;
const TREE: u16 = 16;

const ROOT_DIR: u64 = 2;

fn le16(b: &[u8], p: usize) -> u16 {
    u16::from_le_bytes([b[p], b[p + 1]])
}

fn le32(b: &[u8], p: usize) -> u32 {
    u32::from_le_bytes(b[p..p + 4].try_into().unwrap())
}

fn le64(b: &[u8], p: usize) -> u64 {
    u64::from_le_bytes(b[p..p + 8].try_into().unwrap())
}

/// The Fletcher-64 checksum of object `b` (all but its first 8 bytes, as 32-bit words).
fn fletcher(b: &[u8]) -> u64 {
    const M: u64 = 0xffff_ffff;
    let (mut s1, mut s2) = (0u64, 0u64);
    for w in b[8..].as_chunks::<4>().0 {
        s1 += u32::from_le_bytes(*w) as u64;
        s2 += s1;
    }
    let (s1, s2) = (s1 % M, s2 % M);
    let c1 = M - (s1 + s2) % M;
    let c2 = M - (s1 + c1) % M;
    c2 << 32 | c1
}

/// The block size of a container superblock `b` (block 0's start) as `starts` accepts it.
fn block_size(b: &[u8]) -> Option<usize> {
    if b.len() < 48 || &b[32..36] != b"NXSB" {
        return None;
    }
    let (size, count) = (le32(b, 36) as usize, le64(b, 40));
    // (the format: 4 KiB to 64 KiB)
    (size.is_power_of_two() && (4096..=65536).contains(&size) && count > 0).then_some(size)
}

/// Whether `d` is an APFS container: its superblock's magic and sizes.
pub fn starts(d: &dyn Image) -> bool {
    d.get(0..48).is_some_and(|b| block_size(&b).is_some())
}

/// What the container superblock tells of a container.
struct Container<'a> {
    d: &'a dyn Image,
    block: usize,
    /// Blocks of the container within the image.
    blocks: u64,
    xid: u64,
    omap: u64,
    fs: Vec<u64>,
    /// Blocks of B-trees still to be read in all: the nodes of the container are read once each.
    budget: std::cell::Cell<u64>,
}

impl<'a> Container<'a> {
    /// The bytes of block `n`, where it is within.
    fn block(&self, n: u64) -> Option<Cow<'a, [u8]>> {
        if n >= self.blocks {
            return None;
        }
        let at = usize::try_from(n).ok()?.checked_mul(self.block)?;
        self.d.get(at..at.checked_add(self.block)?)
    }

    /// Container superblock in block `n`, where it is a valid one.
    fn superblock(d: &'a dyn Image, block: usize, n: u64) -> Option<Container<'a>> {
        let at = usize::try_from(n).ok()?.checked_mul(block)?;
        let b = d.get(at..at.checked_add(block)?)?;
        if block_size(&b)? != block || le32(&b, 24) & 0xffff != 1 || fletcher(&b) != le64(&b, 0) {
            return None;
        }
        let count = le64(&b, 40);
        let blocks = count.min((d.len() / block) as u64);
        let max = (le32(&b, 180) as usize).min(100);
        let fs = (0..max).map(|i| le64(&b, 184 + 8 * i)).filter(|&o| o != 0).collect();
        Some(Container { d, block, blocks, xid: le64(&b, 16), omap: le64(&b, 160), fs, budget: blocks.into() })
    }
}

/// The newest valid container superblock of `d`: those of the checkpoint descriptor area (when it
/// is contiguous) and block 0.
fn container(d: &dyn Image) -> Option<Container<'_>> {
    let b = d.get(0..48)?;
    let block = block_size(&b)?;
    let first = d.get(0..block)?;
    let mut best = Container::superblock(d, block, 0);
    let (desc, base) = (le32(&first, 104), le64(&first, 112));
    if desc & 0x8000_0000 == 0 {
        let blocks = (d.len() / block) as u64;
        for n in base..base.saturating_add(desc as u64).min(blocks) {
            if let Some(c) = Container::superblock(d, block, n)
                && best.as_ref().is_none_or(|b| c.xid > b.xid)
            {
                best = Some(c);
            }
        }
    }
    best
}

/// An entry of a B-tree node: key and value.
type Entry<'b> = (&'b [u8], &'b [u8]);

/// A B-tree node `b` (a block): its level and flags, and its entries (those within it).
fn node(b: &[u8]) -> Option<(u16, u16, Vec<Entry<'_>>)> {
    if b.len() < 56 {
        return None;
    }
    let (flags, level, nkeys) = (le16(b, 32), le16(b, 34), le32(b, 36) as usize);
    let (toff, tlen) = (le16(b, 40) as usize, le16(b, 42) as usize);
    let table = 56 + toff;
    let keys = table + tlen;
    let root = flags & 1 != 0;
    let leaf = flags & 2 != 0;
    let fixed = flags & 4 != 0;
    let end = if root { b.len().checked_sub(40)? } else { b.len() };
    let width = if fixed { 4 } else { 8 };
    if level > TREE || (leaf != (level == 0)) || keys > end || nkeys.checked_mul(width)? > tlen {
        return None;
    }
    let mut out = Vec::with_capacity(nkeys);
    for i in 0..nkeys {
        let t = table + i * width;
        let (ko, kl, vo, vl) = if fixed {
            (le16(b, t) as usize, 16, le16(b, t + 2) as usize, if leaf { 16 } else { 8 })
        } else {
            (le16(b, t) as usize, le16(b, t + 2) as usize, le16(b, t + 4) as usize, le16(b, t + 6) as usize)
        };
        // (0xffff: no entry)
        if ko == 0xffff || vo == 0xffff {
            continue;
        }
        let (Some(k), Some(v)) = (b.get(keys + ko..).and_then(|k| k.get(..kl)), end.checked_sub(vo)) else { continue };
        let Some(v) = b[..end].get(v..).and_then(|v| v.get(..vl)) else { continue };
        out.push((k, v));
    }
    Some((level, flags, out))
}

impl Container<'_> {
    /// The entries of the leaves of the B-tree at block `root` (`resolve` gives a child's block of
    /// its value), each node read once; `f` takes them.
    fn walk(&self, root: u64, resolve: &dyn Fn(u64) -> Option<u64>, f: &mut dyn FnMut(&[u8], &[u8])) {
        let mut seen = HashSet::new();
        let mut todo = vec![(root, TREE + 1)];
        while let Some((n, above)) = todo.pop() {
            if self.budget.get() == 0 || !seen.insert(n) {
                continue;
            }
            self.budget.set(self.budget.get() - 1);
            let Some(b) = self.block(n) else { continue };
            let Some((level, _, entries)) = node(&b) else { continue };
            if level >= above {
                continue;
            }
            for (k, v) in entries {
                if level == 0 {
                    f(k, v);
                } else if v.len() == 8
                    && let Some(c) = resolve(le64(v, 0))
                {
                    todo.push((c, level));
                }
            }
        }
    }

    /// The object map at block `n`: for each virtual oid the block of its newest version of an xid
    /// up to `xid` (not deleted).
    fn omap(&self, n: u64, xid: u64) -> HashMap<u64, u64> {
        let Some(b) = self.block(n) else { return HashMap::new() };
        if b.len() < 56 {
            return HashMap::new();
        }
        let tree = le64(&b, 48);
        let mut found: HashMap<u64, (u64, u32, u64)> = HashMap::new();
        self.walk(tree, &Some, &mut |k, v| {
            if k.len() < 16 || v.len() < 16 {
                return;
            }
            let (oid, x) = (le64(k, 0), le64(k, 8));
            if x <= xid && found.get(&oid).is_none_or(|e| e.0 < x) {
                found.insert(oid, (x, le32(v, 0), le64(v, 8)));
            }
        });
        found.into_iter().filter(|(_, e)| e.1 & 1 == 0).map(|(o, e)| (o, e.2)).collect()
    }
}

/// An inode of a volume's tree.
struct Inode {
    /// The id of its data stream.
    stream: u64,
    mode: u16,
    bsd_flags: u32,
    size: u64,
}

/// What a file-system tree holds that is read: inodes, directory entries (by parent, with the name
/// and the id of the inode), and file extents (stream, logical byte, length, physical block).
#[derive(Default)]
struct Tree {
    inodes: HashMap<u64, Inode>,
    entries: HashMap<u64, Vec<(Vec<u8>, u64)>>,
    extents: Vec<(u64, u64, u64, u64)>,
}

/// The size of the data stream in extended fields `x` of an inode (`num` fields, then their data).
fn dstream_size(x: &[u8]) -> Option<u64> {
    if x.len() < 4 {
        return None;
    }
    let num = le16(x, 0) as usize;
    let mut at = 4 + 4 * num;
    for i in 0..num {
        let (ty, len) = (*x.get(4 + 4 * i)?, le16(x, 6 + 4 * i) as usize);
        if ty == 8 && len >= 8 {
            return Some(le64(x.get(at..at + 8)?, 0));
        }
        at += len.next_multiple_of(8);
    }
    None
}

/// The name of a directory record's key `k`, hashed (a length of 10 bits and a hash) or not (a
/// length of 16 bits), ended by a zero byte.
fn name(k: &[u8]) -> Option<&[u8]> {
    let fits = |at: usize, n: usize| k.get(at..at + n).filter(|_| at + n == k.len());
    let hashed = if k.len() >= 12 { fits(12, (le32(k, 8) & 0x3ff) as usize) } else { None };
    let n = hashed.or_else(|| if k.len() >= 10 { fits(10, le16(k, 8) as usize) } else { None })?;
    Some(n.strip_suffix(&[0]).unwrap_or(n))
}

impl Tree {
    /// The records of the tree at block `root`.
    fn read(c: &Container, root: u64, vomap: &HashMap<u64, u64>) -> Tree {
        let mut t = Tree::default();
        c.walk(root, &|o| vomap.get(&o).copied(), &mut |k, v| {
            if k.len() < 8 {
                return;
            }
            let h = le64(k, 0);
            let (id, ty) = (h & ((1 << 60) - 1), h >> 60);
            match ty {
                3 if v.len() >= 92 => {
                    let size = dstream_size(&v[92..]).unwrap_or(0);
                    let (stream, bsd_flags, mode) = (le64(v, 8), le32(v, 68), le16(v, 80));
                    t.inodes.insert(id, Inode { stream, mode, bsd_flags, size });
                }
                8 if k.len() >= 16 && v.len() >= 16 => {
                    let len = le64(v, 0) & 0x00ff_ffff_ffff_ffff;
                    t.extents.push((id, le64(k, 8), len, le64(v, 8)));
                }
                9 if v.len() >= 10 => {
                    if let Some(n) = name(k).filter(|n| !n.is_empty() && *n != b"." && *n != b"..") {
                        t.entries.entry(id).or_default().push((n.to_vec(), le64(v, 0)));
                    }
                }
                _ => {}
            }
        });
        t.extents.sort_unstable();
        t
    }

    /// The byte ranges of the blocks of `stream` up to `size` bytes (holes counted, not taken).
    fn ranges(&self, stream: u64, size: u64, block: usize) -> Vec<Range<usize>> {
        let from = self.extents.partition_point(|e| e.0 < stream);
        let mut out: Vec<Range<usize>> = Vec::new();
        let mut next = 0;
        for &(s, logical, len, physical) in &self.extents[from..] {
            // (extents in order and not overlapping, within the size)
            if s != stream || logical >= size {
                break;
            }
            if logical < next {
                continue;
            }
            next = logical.saturating_add(len);
            let len = len.min(size - logical);
            let Some(start) = physical.checked_mul(block as u64).and_then(|s| usize::try_from(s).ok()) else {
                continue;
            };
            if physical == 0 {
                continue;
            }
            let end = start.saturating_add(len as usize);
            match out.last_mut() {
                Some(r) if r.end == start => r.end = end,
                _ => out.push(start..end),
            }
        }
        out
    }
}

/// The files of the volume of virtual oid `oid` of `c`: none where it is encrypted, sealed or
/// does not read.
fn volume(c: &Container, comap: &HashMap<u64, u64>, oid: u64, out: &mut Vec<File>) {
    let Some(b) = comap.get(&oid).and_then(|&n| c.block(n)) else { return };
    // (APSB; unencrypted; not sealed)
    if b.len() < 272 || &b[32..36] != b"APSB" || le64(&b, 264) & 1 == 0 || le64(&b, 56) & 0x20 != 0 {
        return;
    }
    let vomap = c.omap(le64(&b, 128), le64(&b, 16));
    let Some(&root) = vomap.get(&le64(&b, 136)) else { return };
    let t = Tree::read(c, root, &vomap);
    let mut seen = HashSet::from([ROOT_DIR]);
    let mut taken = HashSet::new();
    let mut todo: Vec<(Vec<u8>, u64, usize)> = vec![(Vec::new(), ROOT_DIR, 0)];
    while let Some((path, dir, depth)) = todo.pop() {
        let Some(list) = t.entries.get(&dir) else { continue };
        for (name, id) in list {
            let Some(i) = t.inodes.get(id) else { continue };
            let full = match path.is_empty() {
                true => name.clone(),
                false => [&path[..], b"/", name].concat(),
            };
            match i.mode & 0o170000 {
                0o040000 => {
                    if depth < DEPTH && seen.insert(*id) {
                        todo.push((full, *id, depth + 1));
                    }
                }
                // (decmpfs: its data is elsewhere; each file once, a hard link's other names skipped)
                0o100000 if i.bsd_flags & 0x20 == 0 && taken.insert(*id) => {
                    let extents = t.ranges(i.stream, i.size, c.block);
                    out.push(File { name: full, extents, rank: 0, compressed: None });
                }
                _ => {}
            }
        }
    }
}

/// The files of APFS container `d`.
pub fn files(d: &dyn Image) -> Vec<File> {
    let Some(c) = container(d) else { return Vec::new() };
    let comap = c.omap(c.omap, c.xid);
    let mut out = Vec::new();
    for &oid in &c.fs {
        volume(&c, &comap, oid, &mut out);
    }
    out
}
