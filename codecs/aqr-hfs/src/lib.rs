//! HFS+ and HFSX volumes read into their files: a file system of aqr-disk ([`FS`]), which takes the
//! image apart into the contents of the files and the rest (the headers, the B-trees, the
//! allocation, attributes and journal, resource forks, free blocks).
//!
//! What it reads (from Apple's Technical Note TN1150, *HFS Plus Volume Format*, the document alone;
//! no implementation's code read): the volume header (at byte 1024); the catalog and extents
//! overflow files from its fork data (the catalog's own overflow extents too); the leaf nodes of the
//! catalog B-tree, by the forward links from the first leaf, each once; its folder and file records;
//! the directories from the root (CNID 2), each once, at most 256 deep. A regular file's contents
//! are its data fork's extents (the 8 of the record, then those of the extents overflow B-tree) up
//! to its logical size. A file hard link (`hlnk`/`hfs+`) is the indirect node file `iNode<N>` of the
//! private directory, reported under the first link's name; the private directory is not walked.
//!
//! Left in the rest: decmpfs-compressed files (`UF_COMPRESSED`), symbolic links, directories,
//! resource forks, the journal (not replayed), the attributes and allocation files, wrapped (HFS
//! "BD") volumes. HFSX reads the same, its names compared by case only in the B-tree's order.

use std::collections::{HashMap, HashSet};
use std::ops::Range;

pub use aqr_disk::{File, Image};

/// HFS+ and HFSX as a file system of aqr-disk.
pub const FS: aqr_disk::FileSystem = aqr_disk::FileSystem { name: "hfs", starts, files };

/// Deepest directory read, from the root.
const DEPTH: usize = 256;
const ROOT: u32 = 2;
const CATALOG_ID: u32 = 4;

fn be16(b: &[u8], p: usize) -> u16 {
    u16::from_be_bytes([b[p], b[p + 1]])
}

fn be32(b: &[u8], p: usize) -> u32 {
    u32::from_be_bytes(b[p..p + 4].try_into().unwrap())
}

fn be64(b: &[u8], p: usize) -> u64 {
    u64::from_be_bytes(b[p..p + 8].try_into().unwrap())
}

/// The block size and count of a volume header `b` (the volume's byte 1024 on) as `starts` accepts
/// it.
fn geometry(b: &[u8]) -> Option<(u64, u64)> {
    if b.len() < 48 {
        return None;
    }
    let (sig, version) = (&b[0..2], be16(b, 2));
    let (size, count) = (be32(b, 40) as u64, be32(b, 44) as u64);
    let signed = (sig == b"H+" && version == 4) || (sig == b"HX" && version == 5);
    (signed && size.is_power_of_two() && size >= 512 && count > 0).then_some((size, count))
}

/// Whether `d` is an HFS+ or HFSX volume: its header's signature, version and sizes.
pub fn starts(d: &dyn Image) -> bool {
    d.get(1024..1024 + 48).is_some_and(|b| geometry(&b).is_some())
}

/// A fork's data: the size and the first 8 extents (start block, count).
struct Fork {
    size: u64,
    ext: [(u32, u32); 8],
}

impl Fork {
    fn parse(b: &[u8]) -> Fork {
        let mut ext = [(0, 0); 8];
        for (i, e) in ext.iter_mut().enumerate() {
            *e = (be32(b, 16 + 8 * i), be32(b, 20 + 8 * i));
        }
        Fork { size: be64(b, 0), ext }
    }
}

/// Extents of the extents overflow file: `(file id, first block)` of the data fork to 8 extents.
type Overflow = HashMap<(u32, u32), [(u32, u32); 8]>;

/// The extents (start block, blocks) of fork `f` of file `id` within the `total` blocks of the
/// volume, as many blocks as `need`: the 8 of the fork, then the overflow records that follow.
fn extents(f: &Fork, id: u32, need: u64, total: u64, overflow: &Overflow) -> Vec<(u64, u64)> {
    let need = need.min(u32::MAX as u64);
    let (mut covered, mut out, mut cur) = (0u64, Vec::new(), f.ext);
    for _ in 0..=overflow.len() {
        for (s, c) in cur {
            let (s, c) = (s as u64, c as u64);
            if c == 0 || covered >= need || s + c > total {
                return out;
            }
            let take = c.min(need - covered);
            out.push((s, take));
            covered += take;
        }
        match u32::try_from(covered).ok().and_then(|c| overflow.get(&(id, c))) {
            Some(next) if covered < need => cur = *next,
            _ => break,
        }
    }
    out
}

/// A B-tree stored in a special file: its extents as byte ranges of the image, the node size.
struct Tree<'a> {
    d: &'a dyn Image,
    /// (offset in the file, range in the image)
    parts: Vec<(u64, Range<usize>)>,
    node: usize,
    first_leaf: u32,
    /// Nodes of the file (the header's count, at most what the file holds).
    nodes: u64,
}

impl<'a> Tree<'a> {
    fn open(d: &'a dyn Image, bs: u64, ext: &[(u64, u64)]) -> Option<Tree<'a>> {
        let (mut parts, mut at) = (Vec::new(), 0u64);
        for &(s, c) in ext {
            let (start, len) = (usize::try_from(s.checked_mul(bs)?).ok()?, usize::try_from(c.checked_mul(bs)?).ok()?);
            parts.push((at, start..start.checked_add(len)?));
            at += len as u64;
        }
        let mut t = Tree { d, parts, node: 512, first_leaf: 0, nodes: 0 };
        let h = t.read(0, 512)?;
        let node = be16(&h, 32) as usize;
        // (kind 1: the header node; nodes of 512 B to 32 KiB)
        if h[8] != 1 || !node.is_power_of_two() || !(512..=65536).contains(&node) {
            return None;
        }
        t.node = node;
        t.first_leaf = be32(&h, 24);
        t.nodes = (be32(&h, 36) as u64).min(at / node as u64);
        Some(t)
    }

    /// Bytes `off..off + len` of the file, where all of it is within.
    fn read(&self, off: u64, len: usize) -> Option<Vec<u8>> {
        let (mut out, mut pos) = (Vec::with_capacity(len), off);
        let end = off.checked_add(len as u64)?;
        for (at, r) in &self.parts {
            let part_end = at + r.len() as u64;
            if pos >= part_end || end <= *at {
                continue;
            }
            if pos < *at {
                return None;
            }
            let from = r.start + (pos - at) as usize;
            let to = r.start + (end.min(part_end) - at) as usize;
            out.extend_from_slice(&self.d.get(from..to)?);
            pos = end.min(part_end);
        }
        (pos == end).then_some(out)
    }

    /// The records of the leaves, from the first leaf by the forward links, each node once.
    fn leaves(&self, f: &mut dyn FnMut(&[u8])) {
        let mut seen = HashSet::new();
        let mut n = self.first_leaf;
        while n != 0 && (n as u64) < self.nodes && seen.len() as u64 <= self.nodes && seen.insert(n) {
            let Some(b) = self.read(n as u64 * self.node as u64, self.node) else { return };
            // (kind -1: a leaf; height 1)
            if b[8] != 0xff || b[9] != 1 {
                return;
            }
            let count = be16(&b, 10) as usize;
            if 14 + 2 * (count + 1) <= b.len() {
                let off = |i: usize| be16(&b, b.len() - 2 * (i + 1)) as usize;
                for i in 0..count {
                    let (s, e) = (off(i), off(i + 1));
                    if s >= 14 && s < e && e <= b.len() - 2 * (count + 1) {
                        f(&b[s..e]);
                    }
                }
            }
            n = be32(&b, 0);
        }
    }
}

/// A file record of the catalog.
struct Rec {
    parent: u32,
    id: u32,
    name: Vec<u8>,
    mode: u16,
    /// The BSD info's owner flags (`UF_COMPRESSED` is 0x20) and special field.
    owner_flags: u8,
    special: u32,
    hard_link: bool,
    data: Fork,
}

/// Directories and files of the catalog.
#[derive(Default)]
struct Catalog {
    /// Folders by id: parent and name.
    folders: HashMap<u32, (u32, Vec<u8>)>,
    files: Vec<Rec>,
}

/// The name of a catalog key `k` (parent id, then a length and UTF-16 units) in UTF-8.
fn key_name(k: &[u8]) -> Option<Vec<u8>> {
    let n = be16(k, 4) as usize;
    let units = k.get(6..6 + 2 * n).filter(|_| n <= 255)?;
    let units: Vec<u16> = units.as_chunks::<2>().0.iter().map(|c| u16::from_be_bytes(*c)).collect();
    Some(String::from_utf16_lossy(&units).into_bytes())
}

impl Catalog {
    fn add(&mut self, rec: &[u8]) {
        if rec.len() < 8 {
            return;
        }
        let keylen = be16(rec, 0) as usize;
        let at = (2 + keylen + 1) & !1;
        if keylen < 6 || rec.len() < at + 2 || 2 + keylen > rec.len() {
            return;
        }
        let Some(name) = key_name(&rec[2..2 + keylen]) else { return };
        let (parent, d) = (be32(rec, 2), &rec[at..]);
        match be16(d, 0) {
            1 if d.len() >= 12 => {
                self.folders.insert(be32(d, 8), (parent, name));
            }
            2 if d.len() >= 248 => {
                let (ty, creator) = (&d[48..52], &d[52..56]);
                self.files.push(Rec {
                    parent,
                    id: be32(d, 8),
                    name,
                    mode: be16(d, 42),
                    owner_flags: d[41],
                    special: be32(d, 44),
                    hard_link: ty == b"hlnk" && creator == b"hfs+",
                    data: Fork::parse(&d[88..168]),
                });
            }
            _ => {}
        }
    }
}

/// The files of the volume `d`.
pub fn files(d: &dyn Image) -> Vec<File> {
    let Some(h) = d.get(1024..1024 + 512) else { return Vec::new() };
    let Some((bs, total)) = geometry(&h) else { return Vec::new() };
    let (extents_fork, catalog_fork) = (Fork::parse(&h[192..272]), Fork::parse(&h[272..352]));

    // the extents overflow file: records of the data forks
    let mut overflow = Overflow::new();
    let none = Overflow::new();
    let ext = extents(&extents_fork, 3, extents_fork.size.div_ceil(bs), total, &none);
    if let Some(t) = Tree::open(d, bs, &ext) {
        t.leaves(&mut |r| {
            // (key: length 10, fork type, pad, file id, first block; then 8 extents)
            if r.len() >= 12 + 64 && be16(r, 0) == 10 && r[2] == 0 {
                let mut e = [(0, 0); 8];
                for (i, e) in e.iter_mut().enumerate() {
                    *e = (be32(r, 12 + 8 * i), be32(r, 16 + 8 * i));
                }
                overflow.insert((be32(r, 4), be32(r, 8)), e);
            }
        });
    }

    let ext = extents(&catalog_fork, CATALOG_ID, catalog_fork.size.div_ceil(bs), total, &overflow);
    let Some(tree) = Tree::open(d, bs, &ext) else { return Vec::new() };
    let mut cat = Catalog::default();
    tree.leaves(&mut |r| cat.add(r));

    // children by folder; the private directory of the hard links' inodes (a name of four NULs first)
    let mut sub: HashMap<u32, Vec<u32>> = HashMap::new();
    let mut files: HashMap<u32, Vec<usize>> = HashMap::new();
    let mut private = None;
    for (&id, (parent, name)) in &cat.folders {
        if id != ROOT {
            sub.entry(*parent).or_default().push(id);
        }
        if *parent == ROOT && name.starts_with(&[0; 4]) {
            private = Some(id);
        }
    }
    for (i, f) in cat.files.iter().enumerate() {
        files.entry(f.parent).or_default().push(i);
    }
    // (in the order of the catalog: names sorted would need the B-tree's compare; ids are stable)
    for v in sub.values_mut() {
        v.sort_unstable();
    }
    let mut inodes: HashMap<u32, usize> = HashMap::new();
    for &i in private.and_then(|p| files.get(&p)).into_iter().flatten() {
        let n = &cat.files[i].name;
        if let Some(num) =
            n.strip_prefix(b"iNode").and_then(|s| std::str::from_utf8(s).ok()).and_then(|s| s.parse().ok())
        {
            inodes.insert(num, i);
        }
    }

    // the directories from the root, depth first, in one path buffer
    enum Item {
        Folder(u32, usize),
        File(usize),
        Pop(usize),
    }
    let (mut out, mut path, mut taken) = (Vec::new(), Vec::<u8>::new(), HashSet::new());
    let mut budget = d.len().saturating_add(1 << 20);
    let mut seen = HashSet::from([ROOT]);
    let mut todo = vec![Item::Folder(ROOT, 0)];
    while let Some(item) = todo.pop() {
        match item {
            Item::Pop(n) => path.truncate(n),
            Item::Folder(id, depth) => {
                todo.push(Item::Pop(path.len()));
                if id != ROOT {
                    path.push(b'/');
                    path.extend_from_slice(&cat.folders[&id].1);
                }
                // (the subfolders are popped first, then the files)
                for &i in files.get(&id).into_iter().flatten() {
                    todo.push(Item::File(i));
                }
                if depth < DEPTH {
                    for &c in sub.get(&id).into_iter().flatten() {
                        if Some(c) != private.filter(|_| id == ROOT) && seen.insert(c) {
                            todo.push(Item::Folder(c, depth + 1));
                        }
                    }
                }
            }
            Item::File(i) => {
                let link = &cat.files[i];
                let rec = if link.hard_link {
                    // (the inode file; each once, under the first link's name)
                    match inodes.get(&link.special) {
                        Some(&n) if taken.insert(link.special) => &cat.files[n],
                        _ => continue,
                    }
                } else {
                    link
                };
                // (regular files, or a mode not set; decmpfs: the data is elsewhere)
                if !matches!(rec.mode & 0o170000, 0 | 0o100000) || rec.owner_flags & 0x20 != 0 {
                    continue;
                }
                let name = &link.name;
                let len = path.len() + name.len() + !path.is_empty() as usize;
                if len > budget {
                    break;
                }
                budget -= len;
                let full = if path.is_empty() {
                    name.clone()
                } else {
                    // (the path buffer starts with the slash of its first folder)
                    [&path[1..], b"/", &name[..]].concat()
                };
                out.push(File { name: full, extents: ranges(d, rec, bs, total, &overflow), rank: 0, compressed: None });
            }
        }
    }
    out
}

/// The byte ranges of the data fork of `r` up to its size (merged where they touch, cut at the
/// image).
fn ranges(d: &dyn Image, r: &Rec, bs: u64, total: u64, overflow: &Overflow) -> Vec<Range<usize>> {
    let mut out: Vec<Range<usize>> = Vec::new();
    let mut left = r.data.size;
    for (s, c) in extents(&r.data, r.id, r.data.size.div_ceil(bs), total, overflow) {
        let (Some(start), len) = (s.checked_mul(bs).and_then(|s| usize::try_from(s).ok()), (c * bs).min(left)) else {
            break;
        };
        left -= len;
        let end = start.saturating_add(len as usize).min(d.len());
        if start >= end {
            continue;
        }
        match out.last_mut() {
            Some(l) if l.end == start => l.end = end,
            _ => out.push(start..end),
        }
    }
    out
}
