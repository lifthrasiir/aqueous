//! The entries of an archive of several files (spec, "Archive model" and "Metadata block"): a
//! [`Tree`] of directories, files, links and special nodes, with the optional metadata that this
//! crate knows of each ([`Extra`]: timestamps, ownership, permissions, flags, extended
//! attributes and forks), and its metadata block.
//!
//! The optional metadata is CBOR: this module writes the little it needs of it, and reads any
//! well-formed CBOR, taking what it knows and leaving the rest.

use std::borrow::Cow;
use std::cmp::Ordering;
use std::collections::HashMap;
use std::sync::Arc;

use aqr_common::varint;

use super::{Decoded, invalid};

/// The entries of an archive, below its root directory.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Tree {
    /// Of the root directory.
    pub root: Extra,
    /// Each after its parent.
    pub entries: Vec<Entry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// The directory it is in (by index in [`Tree::entries`]), or the root.
    pub parent: Option<usize>,
    /// The name as an entry name is encoded ([`name_from`]): a non-empty WTF-8 string, or
    /// `C1` and any bytes.
    pub name: Vec<u8>,
    pub node: EntryNode,
    pub extra: Extra,
}

impl Entry {
    /// A tombstone of `name` in directory `parent`: what is at that name in the segments before
    /// is taken away.
    pub fn tombstone(parent: Option<usize>, name: Vec<u8>) -> Entry {
        Entry { parent, name, node: EntryNode::TOMBSTONE, extra: Extra::default() }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntryNode {
    Dir,
    /// A file of a content (by index, among the contents archived).
    File(usize),
    /// A symbolic link, its target encoded as a name is.
    Link(Vec<u8>),
    Fifo,
    /// A character device (or a block one, where `block`) of its device number.
    Device {
        block: bool,
        major: u64,
        minor: u64,
    },
    /// A special entry of a kind undefined (`special_kind` beyond 4), as an archive decoded
    /// holds it, not encoded; or, of kind 0, a tombstone to encode (spec, "Merging segments": what
    /// is at its name in the segments before taken away, it of no extra), as one decoded is
    /// among `SegmentTree::tombstones` rather than an entry.
    Special(u64),
}

impl EntryNode {
    /// The special entry of kind 0: a tombstone.
    pub const TOMBSTONE: EntryNode = EntryNode::Special(0);
}

/// An extended attribute: its name (as `aqr-text`'s bytes) and value (shared by the entries of
/// the same as they are read).
pub type Xattr = (Arc<[u8]>, Arc<[u8]>);

/// What an entry has of the optional metadata, as far as this crate knows it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Extra {
    pub crtime: Option<Time>,
    pub ctime: Option<Time>,
    pub mtime: Option<Time>,
    pub atime: Option<Time>,
    pub arctime: Option<Time>,
    /// `link-group` (of files alone).
    pub link_group: Option<i128>,
    /// `xattrs`.
    pub xattrs: Vec<Xattr>,
    /// `forks`: names (as `aqr-text`'s bytes) and contents (by index, as [`EntryNode::File`]).
    pub forks: Vec<(Vec<u8>, usize)>,
    /// `win-link-type` (of links alone): a link to a file (0), to a directory (1), or a
    /// junction (2).
    pub win_link_type: Option<u8>,
    /// `win-attrs` ([`WIN_ATTRS_ALLOWED`]).
    pub win_attrs: Option<u32>,
    /// `unix-mode`: the permission bits (0o7777).
    pub mode: Option<u32>,
    pub uid: Option<u64>,
    pub gid: Option<u64>,
    /// `unix-uname` and `unix-gname` (as `aqr-text`'s bytes).
    pub uname: Option<Arc<[u8]>>,
    pub gname: Option<Arc<[u8]>>,
    /// `mac-flags` ([`MAC_FLAGS_ALLOWED`]).
    pub mac_flags: Option<u32>,
}

impl Extra {
    pub fn is_empty(&self) -> bool {
        *self == Extra::default()
    }

    /// Of the times, only the points in time: the local times of an unknown time zone left out,
    /// as what applies them to files takes them.
    pub fn instants(&self) -> Cow<'_, Extra> {
        let times = [self.crtime, self.ctime, self.mtime, self.atime, self.arctime];
        if times.iter().flatten().all(Time::is_instant) {
            return Cow::Borrowed(self);
        }
        let mut x = self.clone();
        for t in [&mut x.crtime, &mut x.ctime, &mut x.mtime, &mut x.atime, &mut x.arctime] {
            *t = t.filter(Time::is_instant);
        }
        Cow::Owned(x)
    }
}

/// The bits `win-attrs` may have: read-only, hidden, system, archive, temporary and not
/// content-indexed.
pub const WIN_ATTRS_ALLOWED: u32 = 0x1 | 0x2 | 0x4 | 0x20 | 0x100 | 0x2000;
/// The bits `mac-flags` may have: `UF_NODUMP`, `UF_IMMUTABLE`, `UF_APPEND`, `UF_OPAQUE`,
/// `UF_HIDDEN`, `SF_ARCHIVED`, `SF_IMMUTABLE`, `SF_APPEND` and `SF_NOUNLINK`.
pub const MAC_FLAGS_ALLOWED: u32 = 0xf | 0x8000 | 0x1_0000 | 0x2_0000 | 0x4_0000 | 0x10_0000;

/// An `aqr-time` this crate reads: seconds and nanoseconds since 1970-01-01T00:00:00Z (a local
/// time in an unknown time zone as if it were UTC), with what it tells of its time zone and its
/// quantization interval.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Time {
    pub secs: i64,
    pub nanos: u32,
    pub zone: Zone,
    /// The quantization interval, in nanoseconds: the scale of its encoding (1, a nanosecond, up
    /// to a day's).
    pub unit: u64,
}

/// What an `aqr-time` tells of its time zone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Zone {
    /// A point in time, of no time zone told.
    Utc,
    /// A point in time, recorded where the offset from UTC was this many 15 minutes (east
    /// positive).
    Offset(i8),
    /// A local time in an unknown time zone: no point in time.
    Local,
}

impl Time {
    /// A point in time of nanoseconds, of no time zone told.
    pub const fn new(secs: i64, nanos: u32) -> Time {
        Time { secs, nanos, zone: Zone::Utc, unit: 1 }
    }

    /// Whether it is a point in time (not a local time of an unknown time zone).
    pub fn is_instant(&self) -> bool {
        self.zone != Zone::Local
    }

    /// The date and time of day as recorded (of UTC where no time zone is told; at its offset
    /// where one is; a local time as it is): year, month, day, hour, minute and second.
    pub fn civil(&self) -> (i64, u32, u32, u32, u32, u32) {
        let secs = match self.zone {
            Zone::Offset(o) => self.secs.saturating_add(900 * o as i64),
            _ => self.secs,
        };
        let (days, s) = (secs.div_euclid(86400), secs.rem_euclid(86400) as u32);
        // (Howard Hinnant's days_from_civil, inverted)
        let z = days + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z.rem_euclid(146_097);
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
        let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
        let y = yoe + era * 400 + (m <= 2) as i64;
        (y, m, d, s / 3600, s / 60 % 60, s % 60)
    }
}

/// The encoding of an entry name or link target of `bytes`: as they are where they are UTF-8
/// (WTF-8 even), else after `C1`.
pub fn name_from(bytes: &[u8]) -> Vec<u8> {
    if wtf8(bytes) { bytes.to_vec() } else { [&[0xc1][..], bytes].concat() }
}

/// The bytes of an encoded name or link target, and whether they were not WTF-8.
pub fn name_bytes(name: &[u8]) -> (&[u8], bool) {
    match name.split_first() {
        Some((0xc1, rest)) => (rest, true),
        _ => (name, false),
    }
}

/// Whether `d` is WTF-8: UTF-8, with surrogates allowed alone (a pair is 4 bytes).
fn wtf8(d: &[u8]) -> bool {
    let mut i = 0;
    let mut high = false;
    while i < d.len() {
        let b = d[i];
        let (n, min) = match b {
            0x00..=0x7f => (1, 0),
            0xc2..=0xdf => (2, 0x80),
            0xe0..=0xef => (3, 0x800),
            0xf0..=0xf4 => (4, 0x1_0000),
            _ => return false,
        };
        let Some(cont) = d.get(i + 1..i + n) else { return false };
        if cont.iter().any(|&c| c & 0xc0 != 0x80) {
            return false;
        }
        let mut c = if n == 1 { b as u32 } else { (b as u32) & (0x7f >> n) };
        cont.iter().for_each(|&x| c = (c << 6) | (x & 0x3f) as u32);
        if c < min || c > 0x10_ffff {
            return false;
        }
        // a high surrogate followed by a low one is a pair, which WTF-8 takes in 4 bytes
        let low = (0xdc00..0xe000).contains(&c);
        if high && low {
            return false;
        }
        high = (0xd800..0xdc00).contains(&c);
        i += n;
    }
    true
}

/// Whether encoded `name` is an entry name the spec allows.
fn valid_name(name: &[u8]) -> bool {
    let (bytes, raw) = name_bytes(name);
    !bytes.is_empty()
        && !bytes.iter().any(|&b| b == 0 || b == b'/')
        && bytes != b"."
        && bytes != b".."
        && (raw || wtf8(bytes))
}

/// Whether encoded `target` is a link target the spec allows.
fn valid_target(target: &[u8]) -> bool {
    let (bytes, raw) = name_bytes(target);
    !bytes.is_empty() && !bytes.contains(&0) && (raw || wtf8(bytes))
}

/// The entries numbered as a metadata block does: directories, then files, then links, each in
/// the order of the tree (so that a directory comes before what it holds). By the entry's
/// index, its number (from 1).
pub(crate) fn numbers(t: &Tree) -> Vec<usize> {
    let mut number = vec![0; t.entries.len()];
    let mut n = 0;
    for pass in 0..3 {
        for (i, e) in t.entries.iter().enumerate() {
            let p = match e.node {
                EntryNode::Dir => 0,
                EntryNode::File(_) => 1,
                _ => 2,
            };
            if p == pass {
                n += 1;
                number[i] = n;
            }
        }
    }
    number
}

/// Checks that `t` is a tree the spec allows: each entry after its parent, which is a
/// directory, of a valid name not taken in it already. (That the first segment has an entry is
/// the writer's to check, and the reader's.)
pub(crate) fn check(t: &Tree, contents: usize) -> Result<(), String> {
    let mut names = std::collections::HashSet::new();
    for (i, e) in t.entries.iter().enumerate() {
        if let Some(p) = e.parent
            && (p >= i || t.entries[p].node != EntryNode::Dir)
        {
            return Err(format!("entry #{i} not after its directory"));
        }
        if !valid_name(&e.name) {
            return Err(format!("entry #{i} of an invalid name"));
        }
        if !names.insert((e.parent, &e.name[..])) {
            return Err(format!("entry #{i} of a name taken"));
        }
        match &e.node {
            EntryNode::File(c) if *c >= contents => return Err(format!("entry #{i} of no content")),
            EntryNode::Link(target) if !valid_target(target) => return Err(format!("entry #{i} of an invalid target")),
            EntryNode::Special(0) if !e.extra.is_empty() => return Err(format!("tombstone #{i} of an extra")),
            EntryNode::Special(0) => {}
            EntryNode::Special(_) => return Err(format!("entry #{i} of a special kind")),
            _ => {}
        }
        if e.extra.link_group.is_some() && !matches!(e.node, EntryNode::File(_)) {
            return Err(format!("entry #{i} in a link group but not a file"));
        }
        if e.extra.win_link_type.is_some() && !matches!(e.node, EntryNode::Link(_)) {
            return Err(format!("entry #{i} of a link type but not a link"));
        }
        check_extra(&e.extra, contents).map_err(|m| format!("entry #{i} of {m}"))?;
    }
    check_extra(&t.root, contents).map_err(|m| format!("the root of {m}"))
}

fn check_extra(x: &Extra, contents: usize) -> Result<(), &'static str> {
    if x.mode.is_some_and(|m| m > 0o7777) {
        return Err("a mode beyond its permissions");
    }
    if x.win_link_type.is_some_and(|t| t > 2) {
        return Err("a link type undefined");
    }
    if x.link_group.is_some_and(|g| !(-(1 << 64)..1 << 64).contains(&g)) {
        return Err("a link group beyond the integers of CBOR");
    }
    if x.win_attrs.is_some_and(|a| a & !WIN_ATTRS_ALLOWED != 0)
        || x.mac_flags.is_some_and(|f| f & !MAC_FLAGS_ALLOWED != 0)
    {
        return Err("attributes or flags not allowed");
    }
    fn names<'a>(mut n: impl Iterator<Item = &'a [u8]>) -> bool {
        let mut seen = std::collections::HashSet::new();
        n.all(|n| !n.is_empty() && seen.insert(n))
    }
    if !names(x.xattrs.iter().map(|a| &a.0[..])) || !names(x.forks.iter().map(|f| &f.0[..])) {
        return Err("an empty or repeated name of an attribute or a fork");
    }
    if x.forks.iter().any(|f| f.1 >= contents) {
        return Err("a fork of no content");
    }
    Ok(())
}

/// A segment as its metadata block tells it, for the archive to be written again segment by
/// segment: its tree, `segment-status` and `journal`.
#[derive(Clone)]
pub(crate) struct SegmentInfo {
    pub tree: SegmentTree,
    pub status: Status,
    pub journal: Journal,
}

/// The tree of a segment as its metadata block tells it (spec, "Interpreting a segment"), before
/// it is merged into those of the segments before it ([`merge`]).
#[derive(Clone)]
pub(crate) struct SegmentTree {
    /// The entries (but tombstones), their contents numbered among those of the segment.
    pub tree: Tree,
    /// Of each content: its leaf (among all of the archive), its output and its size.
    pub refs: Vec<(usize, usize, u64)>,
    /// Whether the root and each entry (by index, after the root's) has an entry extra (not null).
    pub given: Vec<bool>,
    /// The tombstones: the directory each is in (by index, or the root) and its name.
    pub tombstones: Vec<(Option<usize>, Vec<u8>)>,
}

/// An entry of a segment, among those given to `resolve`: the segment (by index) and the
/// entry (by its index among the segment's).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Origin {
    pub segment: usize,
    pub entry: usize,
}

/// What became of an entry of a segment, segments merged (spec, "Merging segments"), where it
/// is not in the tree as it is: each by the entry that did it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fate {
    /// Replaced by an entry of its name (a directory by one not a directory, and anything else
    /// by anything but a tombstone).
    Replaced(Origin),
    /// Taken away by a tombstone.
    Removed(Origin),
    /// A directory merged into a later one of its name, which took its place (and its extra,
    /// where it has one); what became of that is that of the later one.
    Merged(Origin),
    /// Taken away with a directory above it, by that directory's entry (its own fate telling
    /// why).
    WithAncestor(Origin),
}

/// An entry of a segment as [`resolve`] takes it.
pub(crate) struct Shape<'a> {
    /// The directory it is in (by its index among the segment's), or the root.
    pub parent: Option<usize>,
    pub name: &'a [u8],
    pub kind: Kind,
    /// Whether it has an entry extra (not null).
    pub given: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Dir,
    Tombstone,
    /// Anything else: a file, a link, a special file.
    Other,
}

/// Segments merged ([`resolve`]): the tree as it is, each entry by the entries of the segments
/// it is of.
pub(crate) struct Resolved {
    /// The segment whose root extra the root has (none: no segment has one).
    pub root: Option<usize>,
    /// The entries, each after its parent.
    pub entries: Vec<Resolution>,
}

pub(crate) struct Resolution {
    /// The directory it is in (by index in [`Resolved::entries`]), or the root.
    pub parent: Option<usize>,
    /// The entry it is (the latest of a directory merged), and the one whose extra it has: of
    /// a directory the latest of those merged with one (else the first), of anything else its
    /// own.
    pub node: Origin,
    pub extra: Origin,
}

/// The entries of segments `segments` (each entry after its parent; `roots` telling whether each
/// segment's root has an entry extra) merged, in their order (spec, "Merging segments"): a
/// directory merges with one of its name before it, its extra replaced only where it has one;
/// any other entry replaces what had its name, a directory with all under it; a tombstone takes
/// away what has its name. `dropped` is told of each entry not in the tree as it is, and why.
pub(crate) fn resolve(roots: &[bool], segments: &[Vec<Shape>], dropped: &mut dyn FnMut(Origin, Fate)) -> Resolved {
    struct Made {
        parent: Option<usize>,
        node: Origin,
        extra: Origin,
        dir: bool,
        alive: bool,
    }
    // the entries made, in their order: each after its parent
    let mut made: Vec<Made> = Vec::new();
    // by directory made (or the root) and name, the entry there
    let mut at: HashMap<(Option<usize>, &[u8]), usize> = HashMap::new();
    let mut root = None;
    for (s, shapes) in segments.iter().enumerate() {
        if roots[s] {
            root = Some(s);
        }
        // by the segment's entry, the entry made or merged into (none: a tombstone)
        let mut index: Vec<Option<usize>> = Vec::with_capacity(shapes.len());
        for (i, sh) in shapes.iter().enumerate() {
            let o = Origin { segment: s, entry: i };
            let parent = sh.parent.map(|p| index[p].expect("a directory"));
            let key = (parent, sh.name);
            if sh.kind == Kind::Tombstone {
                if let Some(x) = at.remove(&key) {
                    made[x].alive = false;
                    dropped(made[x].node, Fate::Removed(o));
                }
                index.push(None);
                continue;
            }
            let existing = at.get(&key).copied();
            if sh.kind == Kind::Dir
                && let Some(x) = existing.filter(|&x| made[x].dir)
            {
                dropped(made[x].node, Fate::Merged(o));
                made[x].node = o;
                if sh.given {
                    made[x].extra = o;
                }
                index.push(Some(x));
                continue;
            }
            if let Some(x) = existing {
                made[x].alive = false;
                dropped(made[x].node, Fate::Replaced(o));
            }
            at.insert(key, made.len());
            index.push(Some(made.len()));
            made.push(Made { parent, node: o, extra: o, dir: sh.kind == Kind::Dir, alive: true });
        }
    }
    // those left (not taken away, nor under one taken away)
    let mut new = vec![None; made.len()];
    // (by entry made, the nearest of it and those above it taken away)
    let mut gone: Vec<Option<usize>> = vec![None; made.len()];
    let mut entries = Vec::new();
    for (k, m) in made.iter().enumerate() {
        let above = m.parent.and_then(|p| gone[p]);
        gone[k] = if m.alive { above } else { Some(k) };
        match above {
            _ if !m.alive => {}
            Some(a) => dropped(m.node, Fate::WithAncestor(made[a].node)),
            None => {
                new[k] = Some(entries.len());
                let parent = m.parent.map(|p| new[p].expect("kept"));
                entries.push(Resolution { parent, node: m.node, extra: m.extra });
            }
        }
    }
    Resolved { root, entries }
}

/// The trees of the segments of an archive, in their order, merged ([`resolve`]), with their
/// contents as one: those of the files left in the tree alone, in their order. Link groups of a
/// segment's own (not negative) are renumbered apart from those of the other segments, those of
/// the first segment kept as they are.
pub(crate) fn merge(mut trees: Vec<SegmentTree>) -> (Tree, Vec<(usize, usize, u64)>) {
    let roots: Vec<bool> = trees.iter().map(|t| t.given[0]).collect();
    let shapes: Vec<Vec<Shape>> = (trees.iter())
        .map(|t| {
            let entries = t.tree.entries.iter().enumerate().map(|(i, e)| Shape {
                parent: e.parent,
                name: &e.name,
                kind: if e.node == EntryNode::Dir { Kind::Dir } else { Kind::Other },
                given: t.given[i + 1],
            });
            let tombstones = (t.tombstones.iter()).map(|(parent, name)| Shape {
                parent: *parent,
                name,
                kind: Kind::Tombstone,
                given: false,
            });
            entries.chain(tombstones).collect()
        })
        .collect();
    let r = resolve(&roots, &shapes, &mut |_, _| {});
    drop(shapes);
    // the contents and link groups of each segment renumbered among those of all
    let mut refs = Vec::new();
    // (the first link group free for those of a segment's own after the first)
    let own = |t: &Tree| t.entries.iter().filter_map(|e| e.extra.link_group).filter(|&g| g >= 0).max();
    let mut free = trees.first().and_then(|t| own(&t.tree)).map_or(0, |g| g + 1);
    for (s, st) in trees.iter_mut().enumerate() {
        let first = refs.len();
        refs.append(&mut st.refs);
        let mut groups = HashMap::new();
        let mut renumber = |x: &mut Extra| {
            for f in &mut x.forks {
                f.1 += first;
            }
            if s > 0
                && let Some(g) = x.link_group.filter(|&g| g >= 0)
            {
                x.link_group = Some(*groups.entry(g).or_insert_with(|| {
                    free += 1;
                    free - 1
                }));
            }
        };
        if st.given[0] {
            renumber(&mut st.tree.root);
        }
        for e in &mut st.tree.entries {
            renumber(&mut e.extra);
            if let EntryNode::File(c) = &mut e.node {
                *c += first;
            }
        }
    }
    let root = r.root.map(|s| std::mem::take(&mut trees[s].tree.root)).unwrap_or_default();
    let mut tree = Tree { root, entries: Vec::with_capacity(r.entries.len()) };
    for m in r.entries {
        let e = &mut trees[m.node.segment].tree.entries[m.node.entry];
        let (name, node) = (std::mem::take(&mut e.name), std::mem::replace(&mut e.node, EntryNode::Dir));
        let extra = std::mem::take(&mut trees[m.extra.segment].tree.entries[m.extra.entry].extra);
        tree.entries.push(Entry { parent: m.parent, name, node, extra });
    }
    drop(trees);
    // the contents the files left take
    let mut taken = vec![None; refs.len()];
    let mut kept = Vec::new();
    let mut take = |c: &mut usize| {
        *c = *taken[*c].get_or_insert_with(|| {
            kept.push(refs[*c]);
            kept.len() - 1
        });
    };
    for x in std::iter::once(&mut tree.root).chain(tree.entries.iter_mut().map(|e| &mut e.extra)) {
        x.forks.iter_mut().for_each(|f| take(&mut f.1));
    }
    for e in &mut tree.entries {
        if let EntryNode::File(c) = &mut e.node {
            take(c);
        }
    }
    (tree, kept)
}

/// What a file's parameters in a metadata block point to: a leaf block (by where it starts in
/// the archive), its output, and the dictionary it takes (by index).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FileParam {
    pub leaf: usize,
    pub output: u64,
    pub size: u64,
    pub dictionary: Option<usize>,
}

/// The journal points of a segment (spec, "`journal`"): each a time (milliseconds since the
/// epoch, UTC) and a label, in the order of their times.
pub type Journal = Vec<(u64, String)>;

/// What a segment's metadata block tells of the writing it ends (spec, "`segment-status`").
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Status {
    /// No footer meant directly after the segment.
    #[default]
    None,
    /// The last segment of a writing that completed.
    Complete,
    /// The end of a commit of a writing that did not complete, and what it left out where known.
    Incomplete { files: Option<u64>, bytes: Option<u64> },
}

/// The internal structure of the metadata block at `at` (in the archive), the one before it at
/// `prev` (the end of the header magic, less 1, where it is the first), of a segment of the
/// module, resource and dictionary blocks at `modules`, `resources` and `dictionaries`; where
/// each content is (by [`EntryNode::File`]); its `segment-status` `status`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn metadata(
    t: &Tree,
    at: usize,
    prev: usize,
    status: Status,
    journal: &[(u64, String)],
    modules: &[usize],
    resources: &[usize],
    dictionaries: &[usize],
    contents: &[FileParam],
) -> Vec<u8> {
    let param = |i: usize| match t.entries[i].node {
        EntryNode::File(c) => contents[c],
        _ => unreachable!("a file's parameters"),
    };
    let number = numbers(t);
    let n = t.entries.len();
    // by number - 1, the entries' indices
    let mut order = vec![0; n];
    number.iter().enumerate().for_each(|(i, &k)| order[k - 1] = i);
    let count = |p: fn(&EntryNode) -> bool| t.entries.iter().filter(|e| p(&e.node)).count();
    let dirs = count(|n| matches!(n, EntryNode::Dir));
    let nfiles = count(|n| matches!(n, EntryNode::File(_)));

    let prev = (at - prev) as u64;
    let mut entries = Vec::new();
    let mut entry_offsets = Vec::new();
    for (k, &i) in order.iter().enumerate() {
        if k > 0 {
            entry_offsets.push(entries.len() as u64);
        }
        let e = &t.entries[i];
        entries.extend_from_slice(&e.name);
        entries.push(0);
        match &e.node {
            EntryNode::Dir => {}
            EntryNode::File(_) => varint::put(&mut entries, param(i).size),
            EntryNode::Link(target) => {
                varint::put(&mut entries, 1);
                entries.extend_from_slice(target);
                entries.push(0);
            }
            EntryNode::Fifo => varint::put(&mut entries, 2),
            &EntryNode::Device { block, major, minor } => {
                varint::put(&mut entries, 3 + block as u64);
                varint::put(&mut entries, major);
                varint::put(&mut entries, minor);
            }
            EntryNode::Special(0) => varint::put(&mut entries, 0),
            EntryNode::Special(_) => unreachable!("checked"),
        }
    }
    let fork_at = |c: usize| {
        let p = contents[c];
        [(at - p.leaf) as u64, p.output, p.size]
    };
    let opt = optional(t, &order, &fork_at, status, journal);

    let mut m = Vec::new();
    varint::put(&mut m, prev);
    let counts = [modules.len(), resources.len(), dictionaries.len(), dirs, nfiles, n - dirs - nfiles];
    for x in counts.into_iter().chain([entries.len(), opt.len()]) {
        varint::put(&mut m, x as u64);
    }
    for &b in modules.iter().chain(resources).chain(dictionaries) {
        put_u(&mut m, prev, (at - b) as u64);
    }
    for &i in order.iter().skip(1) {
        put_u(&mut m, dirs as u64, t.entries[i].parent.map_or(0, |p| number[p]) as u64);
    }
    for &i in order.iter().skip(dirs).take(nfiles) {
        let p = param(i);
        let offset = (at - p.leaf) as u64;
        // (1-based, zero for none: zero-sized where the segment has no dictionary)
        put_u(&mut m, dictionaries.len() as u64, p.dictionary.map_or(0, |d| d as u64 + 1));
        put_u(&mut m, prev, offset + (p.output > 0) as u64);
        if p.output > 0 {
            varint::put(&mut m, p.output - 1);
        }
    }
    for o in entry_offsets {
        put_u(&mut m, entries.len() as u64, o);
    }
    m.extend_from_slice(&entries);
    m.extend_from_slice(&opt);
    m
}

/// Bytes of `u(n)`.
fn width(n: u64) -> usize {
    match n {
        0 => 0,
        1..0x100 => 1,
        0x100..0x1_0000 => 2,
        0x1_0000..0x1_0000_0000 => 4,
        _ => 8,
    }
}

/// Appends `v` as `u(n)`.
fn put_u(out: &mut Vec<u8>, n: u64, v: u64) {
    debug_assert!(v <= n);
    out.extend_from_slice(&v.to_le_bytes()[..width(n)]);
}

/// The optional metadata of `t`, its entries in `order`, its forks' contents where `fork_at`
/// tells (`aqr-fork`: offset, output and size): `entry-extras` where any entry has an extra, and
/// `segment-status` where `status` is not [`Status::None`]; else nothing (an empty map).
fn optional(
    t: &Tree,
    order: &[usize],
    fork_at: &dyn Fn(usize) -> [u64; 3],
    status: Status,
    journal: &[(u64, String)],
) -> Vec<u8> {
    let extras: Vec<&Extra> = std::iter::once(&t.root).chain(order.iter().map(|&i| &t.entries[i].extra)).collect();
    let last = extras.iter().rposition(|e| !e.is_empty());
    let told = status != Status::None;
    if last.is_none() && !told && journal.is_empty() {
        return Vec::new();
    }
    let mut c = Vec::new();
    head(&mut c, 5, last.is_some() as u64 + told as u64 + !journal.is_empty() as u64);
    if let Some(last) = last {
        int(&mut c, ENTRY_EXTRAS);
        head(&mut c, 4, last as u64 + 1);
        for e in &extras[..=last] {
            if e.is_empty() {
                c.push(NULL);
                continue;
            }
            extra(&mut c, e, fork_at);
        }
    }
    match status {
        Status::None => {}
        Status::Complete => {
            int(&mut c, SEGMENT_STATUS);
            c.push(TRUE);
        }
        Status::Incomplete { files, bytes } => {
            int(&mut c, SEGMENT_STATUS);
            head(&mut c, 5, files.is_some() as u64 + bytes.is_some() as u64);
            for (k, v) in [(INCOMPLETE_FILES, files), (INCOMPLETE_BYTES, bytes)] {
                if let Some(v) = v {
                    int(&mut c, k);
                    head(&mut c, 0, v);
                }
            }
        }
    }
    if !journal.is_empty() {
        int(&mut c, JOURNAL);
        head(&mut c, 5, journal.len() as u64);
        for (time, label) in journal {
            head(&mut c, 0, *time);
            head(&mut c, 3, label.len() as u64);
            c.extend_from_slice(label.as_bytes());
        }
    }
    c
}

/// An `aqr-entry-extra` of `e`.
fn extra(out: &mut Vec<u8>, e: &Extra, fork_at: &dyn Fn(usize) -> [u64; 3]) {
    // (the members, counted as they are written)
    let mut m = Vec::new();
    let mut n = 0;
    let mut key = |m: &mut Vec<u8>, k: i128| {
        n += 1;
        int(m, k);
    };
    for (k, t) in [(CRTIME, e.crtime), (CTIME, e.ctime), (MTIME, e.mtime), (ATIME, e.atime), (ARCTIME, e.arctime)] {
        if let Some(t) = t {
            key(&mut m, k);
            time_of(&mut m, t);
        }
    }
    if let Some(g) = e.link_group {
        key(&mut m, LINK_GROUP);
        int(&mut m, g);
    }
    if !e.xattrs.is_empty() {
        key(&mut m, XATTRS);
        head(&mut m, 5, e.xattrs.len() as u64);
        for (name, value) in &e.xattrs {
            text(&mut m, name);
            head(&mut m, 2, value.len() as u64);
            m.extend_from_slice(value);
        }
    }
    if !e.forks.is_empty() {
        key(&mut m, FORKS);
        head(&mut m, 5, e.forks.len() as u64);
        for (name, c) in &e.forks {
            text(&mut m, name);
            head(&mut m, 4, 3);
            fork_at(*c).into_iter().for_each(|v| head(&mut m, 0, v));
        }
    }
    let uints = [
        (WIN_LINK_TYPE, e.win_link_type.map(u64::from)),
        (WIN_ATTRS, e.win_attrs.map(u64::from)),
        (UNIX_MODE, e.mode.map(u64::from)),
        (UNIX_UID, e.uid),
        (UNIX_GID, e.gid),
    ];
    for (k, v) in uints {
        if let Some(v) = v {
            key(&mut m, k);
            head(&mut m, 0, v);
        }
    }
    for (k, name) in [(UNIX_UNAME, &e.uname), (UNIX_GNAME, &e.gname)] {
        if let Some(name) = name {
            key(&mut m, k);
            text(&mut m, name);
        }
    }
    if let Some(f) = e.mac_flags {
        key(&mut m, MAC_FLAGS);
        head(&mut m, 0, f as u64);
    }
    head(out, 5, n);
    out.extend_from_slice(&m);
}

/// An `aqr-time` of `t`, at the precision of nanoseconds (that of the file systems whose times
/// this crate is given); in seconds where those do not fit an integer of CBOR.
fn time_of(out: &mut Vec<u8>, t: Time) {
    let ns = t.secs as i128 * 1_000_000_000 + t.nanos as i128;
    // (a day has no offset: of seconds then)
    let unit = match t.zone {
        Zone::Offset(_) if t.unit == DAY => 1_000_000_000,
        _ => t.unit as i128,
    };
    match t.zone {
        Zone::Utc => {}
        Zone::Offset(_) => head(out, 4, 2),
        Zone::Local => head(out, 4, 1),
    }
    let scaled = TAG_SCALES
        .iter()
        .find(|s| s.1 == unit && ns % unit == 0 && (-(1i128 << 64)..1i128 << 64).contains(&(ns / unit)));
    match scaled {
        Some(_) if unit == 1_000_000_000 => int(out, ns / unit),
        Some(&(tag, unit)) => {
            head(out, 6, tag);
            int(out, ns / unit);
        }
        None if (-(1i128 << 64)..1i128 << 64).contains(&ns) => {
            head(out, 6, TAG_NS);
            int(out, ns);
        }
        None => int(out, t.secs as i128),
    }
    if let Zone::Offset(o) = t.zone {
        int(out, o as i128);
    }
}

/// An `aqr-text` of `b`: a text string where it is UTF-8, WTF-8 (tagged) where it is that, and
/// a byte string otherwise.
fn text(out: &mut Vec<u8>, b: &[u8]) {
    if std::str::from_utf8(b).is_ok() {
        head(out, 3, b.len() as u64);
    } else {
        if wtf8(b) {
            head(out, 6, TAG_WTF8);
        }
        head(out, 2, b.len() as u64);
    }
    out.extend_from_slice(b);
}

const ENTRY_EXTRAS: i128 = -2;
const SEGMENT_STATUS: i128 = -3;
const JOURNAL: i128 = -4;
const INCOMPLETE_FILES: i128 = 0;
const INCOMPLETE_BYTES: i128 = 1;
const CRTIME: i128 = -1;
const CTIME: i128 = -2;
const MTIME: i128 = -3;
const ATIME: i128 = -4;
const ARCTIME: i128 = -5;
const LINK_GROUP: i128 = -7;
const XATTRS: i128 = -10;
const FORKS: i128 = -50;
const WIN_LINK_TYPE: i128 = -60;
const WIN_ATTRS: i128 = -61;
const UNIX_MODE: i128 = -70;
const UNIX_UID: i128 = -71;
const UNIX_GID: i128 = -72;
const UNIX_UNAME: i128 = -73;
const UNIX_GNAME: i128 = -74;
const MAC_FLAGS: i128 = -80;
const NULL: u8 = 0xf6;
const TRUE: u8 = 0xf5;
const TAG_WTF8: u64 = 273;
/// The tags of `$aqr-instant` and `#6.100`, by their scale in nanoseconds.
const TAG_NS: u64 = 126;
/// A day, in nanoseconds: the scale of tag 100.
const DAY: u64 = 86400 * 1_000_000_000;
const TAG_SCALES: [(u64, i128); 8] = [
    (100, DAY as i128),
    (121, 2_000_000_000),
    // (untagged)
    (u64::MAX, 1_000_000_000),
    (122, 10_000_000),
    (123, 1_000_000),
    (124, 1_000),
    (125, 100),
    (126, 1),
];

/// A CBOR head of major type `major` and argument `v`, in the shortest encoding.
fn head(out: &mut Vec<u8>, major: u8, v: u64) {
    let m = major << 5;
    match v {
        0..24 => out.push(m | v as u8),
        24..0x100 => out.extend_from_slice(&[m | 24, v as u8]),
        0x100..0x1_0000 => {
            out.push(m | 25);
            out.extend_from_slice(&(v as u16).to_be_bytes());
        }
        0x1_0000..0x1_0000_0000 => {
            out.push(m | 26);
            out.extend_from_slice(&(v as u32).to_be_bytes());
        }
        _ => {
            out.push(m | 27);
            out.extend_from_slice(&v.to_be_bytes());
        }
    }
}

/// A CBOR integer (within 64 bits, positive or negative).
fn int(out: &mut Vec<u8>, v: i128) {
    if v >= 0 { head(out, 0, v as u64) } else { head(out, 1, (-1 - v) as u64) }
}

/// A CBOR data item, as far as it is read, without allocating but for strings in chunks: strings
/// borrowed from the data, and arrays, maps and tagged items as the bytes of their contents
/// ([`Items`]), checked as they are read first and read again where looked into.
#[derive(Debug, Clone)]
enum Cbor<'a> {
    Int(i128),
    Bytes(Cow<'a, [u8]>),
    Text(Cow<'a, [u8]>),
    Array(Items<'a>),
    /// Its keys and values, one after the other.
    Map(Items<'a>),
    Tag(u64, Items<'a>),
    /// Simple values (null among them), and floats by their bits (as keys are told apart).
    Simple(u8),
    Float(u64),
}

/// The items of an array or a map (keys and values, one after the other) or the item of a tag,
/// checked already: `d`, the bytes of all of them (a break after them where of an indefinite
/// length), and their number (`None`: up to the break), each at `depth`.
#[derive(Debug, Clone, Copy)]
struct Items<'a> {
    d: &'a [u8],
    n: Option<usize>,
    depth: usize,
}

impl<'a> Items<'a> {
    fn iter(self) -> impl Iterator<Item = Cbor<'a>> {
        let (mut at, mut k) = (0, 0);
        std::iter::from_fn(move || {
            if ended(self.d, &mut at, self.n, k) {
                return None;
            }
            k += 1;
            Some(item(self.d, &mut at, self.depth).expect("an item checked"))
        })
    }

    /// Of a map, its pairs.
    fn pairs(self) -> impl Iterator<Item = (Cbor<'a>, Cbor<'a>)> {
        let mut items = self.iter();
        std::iter::from_fn(move || Some((items.next()?, items.next().expect("a map of pairs"))))
    }

    /// Of a tag, its item.
    fn first(self) -> Cbor<'a> {
        self.iter().next().expect("a tag of an item")
    }
}

impl PartialEq for Cbor<'_> {
    fn eq(&self, other: &Self) -> bool {
        use Cbor::*;
        let items = |a: Items, b: Items| {
            let (mut a, mut b) = (a.iter(), b.iter());
            loop {
                match (a.next(), b.next()) {
                    (None, None) => return true,
                    (Some(x), Some(y)) if x == y => {}
                    _ => return false,
                }
            }
        };
        match (self, other) {
            (Int(a), Int(b)) => a == b,
            (Bytes(a), Bytes(b)) | (Text(a), Text(b)) => a == b,
            (Array(a), Array(b)) | (Map(a), Map(b)) => items(*a, *b),
            (Tag(s, a), Tag(t, b)) => s == t && items(*a, *b),
            (Simple(a), Simple(b)) => a == b,
            (Float(a), Float(b)) => a == b,
            _ => false,
        }
    }
}

impl Cbor<'_> {
    /// An order of items, the same as `==` where equal: by kind, then by value (arrays, maps
    /// and tags by their items, in turn).
    fn order(&self, other: &Self) -> Ordering {
        use Cbor::*;
        let rank = |c: &Cbor| match c {
            Int(_) => 0,
            Bytes(_) => 1,
            Text(_) => 2,
            Array(_) => 3,
            Map(_) => 4,
            Tag(..) => 5,
            Simple(_) => 6,
            Float(_) => 7,
        };
        let items = |a: Items, b: Items| {
            let (mut a, mut b) = (a.iter(), b.iter());
            loop {
                match (a.next(), b.next()) {
                    (None, None) => return Ordering::Equal,
                    (None, Some(_)) => return Ordering::Less,
                    (Some(_), None) => return Ordering::Greater,
                    (Some(x), Some(y)) => match x.order(&y) {
                        Ordering::Equal => {}
                        o => return o,
                    },
                }
            }
        };
        match (self, other) {
            (Int(a), Int(b)) => a.cmp(b),
            (Bytes(a), Bytes(b)) | (Text(a), Text(b)) => a.cmp(b),
            (Array(a), Array(b)) | (Map(a), Map(b)) => items(*a, *b),
            (Tag(s, a), Tag(t, b)) => s.cmp(t).then_with(|| items(*a, *b)),
            (Simple(a), Simple(b)) => a.cmp(b),
            (Float(a), Float(b)) => a.cmp(b),
            (a, b) => rank(a).cmp(&rank(b)),
        }
    }
}

/// Keys of a map compared with each before as they are added while there are no more than
/// this, and all sorted once read where there are more.
const FEW_KEYS: usize = 16;

/// Adds key `k` to `keys`, the keys of a map read so far: a duplicate of one before is a
/// failure, found here while [`FEW_KEYS`] at most, else by [`no_duplicates`] once all are added.
fn add_key<'a>(keys: &mut Vec<Cbor<'a>>, k: Cbor<'a>) -> Decoded<()> {
    if keys.len() < FEW_KEYS && keys.contains(&k) {
        return invalid("duplicate keys in the optional metadata");
    }
    keys.push(k);
    Ok(())
}

/// Whether `keys` (of a map, all of them, by [`add_key`]) are of no duplicates: sorted where
/// there are many.
fn no_duplicates(keys: &mut [Cbor]) -> Decoded<()> {
    if keys.len() > FEW_KEYS {
        keys.sort_unstable_by(|a, b| a.order(b));
        if keys.windows(2).any(|w| w[0] == w[1]) {
            return invalid("duplicate keys in the optional metadata");
        }
    }
    Ok(())
}

/// Maps and arrays nested deeper than this are not read (a resource failure).
const CBOR_DEPTH: usize = 64;

/// The head of a CBOR data item at `*at` of `d`, read: its major type, additional information
/// and argument (`None` for an indefinite length).
fn head_of(d: &[u8], at: &mut usize) -> Decoded<(u8, u8, Option<u64>)> {
    let bad = || invalid("bad CBOR in the optional metadata");
    let Some(&b) = d.get(*at) else { return bad() };
    *at += 1;
    let (major, ai) = (b >> 5, b & 0x1f);
    let arg = match ai {
        0..24 => Some(ai as u64),
        24..28 => {
            let n = 1 << (ai - 24);
            let Some(bytes) = d.get(*at..*at + n) else { return bad() };
            *at += n;
            Some(bytes.iter().fold(0u64, |v, &x| (v << 8) | x as u64))
        }
        31 if matches!(major, 2..=5) => None,
        _ => return bad(),
    };
    Ok((major, ai, arg))
}

/// The number of items (`None`: up to a break) of an array or map of argument `arg` at `at` of
/// `d`: no more than the bytes left.
fn count(d: &[u8], at: usize, arg: Option<u64>) -> Decoded<Option<usize>> {
    match arg {
        Some(n) => usize::try_from(n)
            .ok()
            .filter(|&n| n <= d.len() - at.min(d.len()))
            .map(Some)
            .ok_or_else(|| super::Failure::Invalid("bad CBOR length".into())),
        None => Ok(None),
    }
}

/// Whether the array or map of `n` items ([`count`]) at `*at` of `d` ends after `k` of them:
/// at a break (passed) where of an indefinite length.
fn ended(d: &[u8], at: &mut usize, n: Option<usize>, k: usize) -> bool {
    match n {
        Some(n) => k == n,
        None if d.get(*at) == Some(&0xff) => {
            *at += 1;
            true
        }
        None => false,
    }
}

/// Goes over the item at `*at` of `d` at `depth` by its heads alone, as an item not read is
/// (spec, "Partial access"): well-formed as far as they tell, but neither its texts nor a map's
/// keys checked.
fn skip(d: &[u8], at: &mut usize, depth: usize) -> Decoded<()> {
    if depth > CBOR_DEPTH {
        return super::resource("optional metadata nested too deep");
    }
    let bad = || invalid("bad CBOR in the optional metadata");
    let (major, ai, arg) = head_of(d, at)?;
    match (major, arg) {
        (0 | 1, Some(_)) => {}
        (2 | 3, Some(n)) => match count(d, *at, Some(n))? {
            Some(n) => *at += n,
            None => unreachable!("a definite length"),
        },
        // (chunks of the same major type, definite each, up to a break)
        (2 | 3, None) => {
            while d.get(*at) != Some(&0xff) {
                match d.get(*at) {
                    Some(&c) if c >> 5 == major && c & 0x1f != 31 => skip(d, at, depth + 1)?,
                    _ => return bad(),
                }
            }
            *at += 1;
        }
        (4 | 5, arg) => {
            let n = count(d, *at, arg)?;
            let mut k = 0;
            while !ended(d, at, n, k) {
                skip(d, at, depth + 1)?;
                if major == 5 {
                    if n.is_none() && d.get(*at) == Some(&0xff) {
                        return bad();
                    }
                    skip(d, at, depth + 1)?;
                }
                k += 1;
            }
        }
        (6, Some(_)) => skip(d, at, depth + 1)?,
        (7, Some(v)) => match ai {
            0..24 | 25..28 => {}
            24 if v >= 32 => {}
            _ => return bad(),
        },
        _ => return bad(),
    }
    Ok(())
}

/// Reads a CBOR data item at `*at` of `d`.
fn item<'a>(d: &'a [u8], at: &mut usize, depth: usize) -> Decoded<Cbor<'a>> {
    if depth > CBOR_DEPTH {
        return super::resource("optional metadata nested too deep");
    }
    let bad = || invalid::<Cbor>("bad CBOR in the optional metadata");
    let (major, ai, arg) = head_of(d, at)?;
    let len = |n: u64, at: usize| usize::try_from(n).ok().filter(|&n| n <= d.len() - at.min(d.len()));
    let string = |b: Cow<'a, [u8]>| if major == 2 { Cbor::Bytes(b) } else { Cbor::Text(b) };
    Ok(match (major, arg) {
        (0, Some(v)) => Cbor::Int(v as i128),
        (1, Some(v)) => Cbor::Int(-1 - v as i128),
        (2 | 3, Some(n)) => {
            let Some(n) = len(n, *at) else { return bad() };
            let b = &d[*at..*at + n];
            if major == 3 && std::str::from_utf8(b).is_err() {
                return bad();
            }
            *at += n;
            string(Cow::Borrowed(b))
        }
        // chunks of the same major type, definite each, up to a break
        (2 | 3, None) => {
            let mut b = Vec::new();
            while d.get(*at) != Some(&0xff) {
                let Some(&c) = d.get(*at) else { return bad() };
                if c >> 5 != major || c & 0x1f == 31 {
                    return bad();
                }
                match item(d, at, depth + 1)? {
                    Cbor::Bytes(c) | Cbor::Text(c) => b.extend_from_slice(&c),
                    _ => unreachable!("a chunk of its major type"),
                }
            }
            *at += 1;
            string(Cow::Owned(b))
        }
        (4 | 5, arg) => {
            let n = count(d, *at, arg)?;
            let start = *at;
            let mut k = 0;
            if major == 4 {
                while !ended(d, at, n, k) {
                    item(d, at, depth + 1)?;
                    k += 1;
                }
            } else {
                // where each of a few keys is, each compared with those before as they are read
                // again (without room made for them); the keys of a map of more, all kept
                let (mut few, mut many) = ([0usize; FEW_KEYS], Vec::new());
                while !ended(d, at, n, k) {
                    let key_at = *at;
                    let (key, _) = pair(d, at, depth, n)?;
                    if k < FEW_KEYS {
                        if few[..k].iter().any(|&q| item(d, &mut { q }, depth + 1).is_ok_and(|q| q == key)) {
                            return invalid("duplicate keys in the optional metadata");
                        }
                        few[k] = key_at;
                    } else {
                        if many.is_empty() {
                            many.extend(few.iter().map(|&q| item(d, &mut { q }, depth + 1).expect("a key read")));
                        }
                        many.push(key);
                    }
                    k += 1;
                }
                no_duplicates(&mut many)?;
            }
            // (the items of a map: its keys and values)
            let n = n.map(|n| if major == 4 { n } else { 2 * n });
            let items = Items { d: &d[start..*at], n, depth: depth + 1 };
            if major == 4 { Cbor::Array(items) } else { Cbor::Map(items) }
        }
        (6, Some(tag)) => {
            let start = *at;
            item(d, at, depth + 1)?;
            Cbor::Tag(tag, Items { d: &d[start..*at], n: Some(1), depth: depth + 1 })
        }
        (7, Some(v)) => match ai {
            0..24 => Cbor::Simple(v as u8),
            // (two-byte simple values below 32 are not well-formed)
            24 if v >= 32 => Cbor::Simple(v as u8),
            25..28 => Cbor::Float(v),
            _ => return bad(),
        },
        _ => return bad(),
    })
}

/// Reads a pair of a map at `depth` of `n` pairs ([`count`]) at `*at` of `d`.
fn pair<'a>(d: &'a [u8], at: &mut usize, depth: usize, n: Option<usize>) -> Decoded<(Cbor<'a>, Cbor<'a>)> {
    let k = item(d, at, depth + 1)?;
    if n.is_none() && d.get(*at) == Some(&0xff) {
        return invalid("bad CBOR in the optional metadata");
    }
    Ok((k, item(d, at, depth + 1)?))
}

/// A fork as `forks` refers to it: the relative offset of its leaf block, its output and its
/// size.
pub(crate) type ForkRef = (u64, u64, u64);

/// The texts and bytes of the optional metadata taken last, each shared by the entries taking
/// the same (most entries of an archive have the same owner and group names, and often the
/// same extended attributes).
struct Shared<'a> {
    recent: Vec<(&'a [u8], Arc<[u8]>)>,
    next: usize,
}

/// How many texts [`Shared`] keeps.
const SHARED: usize = 16;

impl<'a> Shared<'a> {
    fn new() -> Self {
        Shared { recent: Vec::with_capacity(SHARED), next: 0 }
    }

    /// `b` as an `Arc`, the one made last of the same bytes where it is kept.
    fn get(&mut self, b: Cow<'a, [u8]>) -> Arc<[u8]> {
        let b = match b {
            Cow::Borrowed(b) => b,
            Cow::Owned(b) => return b.into(),
        };
        if let Some((_, a)) = self.recent.iter().find(|(r, _)| *r == b) {
            return a.clone();
        }
        let a: Arc<[u8]> = b.into();
        if self.recent.len() < SHARED {
            self.recent.push((b, a.clone()));
        } else {
            self.recent[self.next] = (b, a.clone());
            self.next = (self.next + 1) % SHARED;
        }
        a
    }
}

/// The extras of the entries (from the root, by number) in optional metadata `d`, of `n`
/// entries besides the root; their forks apart, as they refer to them (their extras' `forks`
/// left empty).
#[cfg(test)]
#[allow(clippy::type_complexity)]
fn extras(d: &[u8], n: usize) -> Decoded<(Vec<Extra>, Vec<Vec<(Vec<u8>, ForkRef)>>)> {
    extras_given(d, n, None).map(|(out, forks, ..)| (out, forks))
}

/// The extras of [`extras`] and their forks, whether each has an entry extra (a map, not null
/// nor left out), and the segment's `segment-status`.
type Extras = (Vec<Extra>, Vec<Vec<(Vec<u8>, ForkRef)>>, Vec<bool>, Status, Journal);

/// [`extras`], and whether each has an entry extra (a map, not null nor left out), and the
/// segment's `segment-status`. Of the entries `wanted` tells (by number from the root, where it
/// is given) alone: the others' entry extras gone over as CBOR, not read (left default, their
/// forks left out).
#[allow(clippy::type_complexity)]
fn extras_given(d: &[u8], n: usize, wanted: Option<&[bool]>) -> Decoded<Extras> {
    let mut out = vec![Extra::default(); n + 1];
    let mut forks = vec![Vec::new(); n + 1];
    let mut given = vec![false; n + 1];
    let mut status = Status::None;
    let mut journal = Journal::new();
    if d.is_empty() {
        return Ok((out, forks, given, status, journal));
    }
    // Walked rather than read whole: the entry-extras an entry at a time, each a member at a
    // time (read whole, those of an archive of many entries took most of opening it).
    let mut at = 0;
    let (major, _, arg) = head_of(d, &mut at)?;
    if major != 5 {
        return invalid("optional metadata not a map");
    }
    let top = count(d, at, arg)?;
    // the keys of the map read (`keys`), and of an entry extra (`members`, made once)
    let (mut keys, mut members): (Vec<Cbor>, Vec<Cbor>) = (Vec::new(), Vec::new());
    let mut shared = Shared::new();
    while !ended(d, &mut at, top, keys.len()) {
        let k = item(d, &mut at, 1)?;
        if top.is_none() && d.get(at) == Some(&0xff) {
            return invalid("bad CBOR in the optional metadata");
        }
        let entry_extras = k == Cbor::Int(ENTRY_EXTRAS);
        let told = k == Cbor::Int(SEGMENT_STATUS);
        let points = k == Cbor::Int(JOURNAL);
        add_key(&mut keys, k)?;
        if told {
            status = status_of(item(d, &mut at, 1)?)?;
            continue;
        }
        if points {
            journal = journal_of(item(d, &mut at, 1)?)?;
            continue;
        }
        if !entry_extras {
            item(d, &mut at, 1)?;
            continue;
        }
        let (major, _, arg) = head_of(d, &mut at)?;
        if major != 4 {
            return invalid("entry-extras not an array");
        }
        let list = count(d, at, arg)?;
        let mut e = 0;
        while !ended(d, &mut at, list, e) {
            if e > n {
                return invalid("more entry-extras than entries");
            }
            let (x, f) = (&mut out[e], &mut forks[e]);
            e += 1;
            if wanted.is_some_and(|w| !w[e - 1]) {
                match d.get(at).map(|&b| b >> 5) {
                    _ if d.get(at) == Some(&0xf6) => at += 1,
                    Some(5) => {
                        given[e - 1] = true;
                        skip(d, &mut at, 2)?;
                    }
                    _ => return invalid("an entry extra not a map"),
                }
                continue;
            }
            match head_of(d, &mut at)? {
                (7, 22, _) => {}
                (5, _, arg) => {
                    given[e - 1] = true;
                    let m = count(d, at, arg)?;
                    members.clear();
                    while !ended(d, &mut at, m, members.len()) {
                        let (k, v) = pair(d, &mut at, 2, m)?;
                        if let Cbor::Int(k) = k {
                            member(x, f, &mut shared, k, v)?;
                        }
                        add_key(&mut members, k)?;
                    }
                    no_duplicates(&mut members)?;
                }
                _ => return invalid("an entry extra not a map"),
            }
        }
    }
    no_duplicates(&mut keys)?;
    if at != d.len() {
        return invalid("bytes after the optional metadata");
    }
    Ok((out, forks, given, status, journal))
}

/// The `journal` of value `v`: times to labels, in the order of the times.
fn journal_of(v: Cbor) -> Decoded<Journal> {
    let Cbor::Map(m) = v else { return invalid("journal not a map") };
    let mut out = Journal::new();
    for (k, v) in m.pairs() {
        let Cbor::Int(t) = k else { return invalid("a journal time not an integer") };
        let t = u64::try_from(t).map_err(|_| super::Failure::Invalid("a journal time out of range".into()))?;
        out.push((t, String::from_utf8_lossy(&aqr_text(v)?).into_owned()));
    }
    out.sort_by_key(|p| p.0);
    if out.windows(2).any(|w| w[0].0 == w[1].0) {
        return invalid("duplicate keys in journal");
    }
    Ok(out)
}

/// The `segment-status` of value `v`: `true`, or an `aqr-incomplete` (other keys let be).
fn status_of(v: Cbor) -> Decoded<Status> {
    let m = match v {
        Cbor::Simple(21) => return Ok(Status::Complete),
        Cbor::Map(m) => m,
        _ => return invalid("segment-status neither true nor a map"),
    };
    let (mut files, mut bytes) = (None, None);
    let mut keys = Vec::new();
    for (k, v) in m.pairs() {
        let field = match k {
            Cbor::Int(INCOMPLETE_FILES) => &mut files,
            Cbor::Int(INCOMPLETE_BYTES) => &mut bytes,
            _ => {
                add_key(&mut keys, k)?;
                continue;
            }
        };
        if field.is_some() {
            return invalid("duplicate keys in segment-status");
        }
        *field = match v {
            Cbor::Int(n) => Some(
                u64::try_from(n).map_err(|_| super::Failure::Invalid("segment-status: a count out of range".into()))?,
            ),
            _ => return invalid("segment-status: a count not an integer"),
        };
    }
    Ok(Status::Incomplete { files, bytes })
}

/// Takes member `k` of value `v` of an entry extra into `x` (its forks into `forks`), where it
/// is one this crate knows, its texts and bytes shared as `shared` has them.
fn member<'a>(
    x: &mut Extra,
    forks: &mut Vec<(Vec<u8>, ForkRef)>,
    shared: &mut Shared<'a>,
    k: i128,
    v: Cbor<'a>,
) -> Decoded<()> {
    let uint = |max: u64| match v {
        Cbor::Int(v) if (0..=max as i128).contains(&v) => Ok(v as u64),
        _ => invalid("a bad unsigned integer in an entry extra"),
    };
    // (`aqr-text`s of names, not empty)
    let name = |c: Cbor<'a>| -> Decoded<Cow<'a, [u8]>> {
        match aqr_text(c)? {
            n if n.is_empty() => invalid("an empty name in an entry extra"),
            n => Ok(n),
        }
    };
    match k {
        CRTIME => x.crtime = time(&v),
        CTIME => x.ctime = time(&v),
        MTIME => x.mtime = time(&v),
        ATIME => x.atime = time(&v),
        ARCTIME => x.arctime = time(&v),
        LINK_GROUP => match v {
            Cbor::Int(g) => x.link_group = Some(g),
            _ => return invalid("a bad link-group"),
        },
        XATTRS => {
            let Cbor::Map(m) = v else { return invalid("xattrs not a map") };
            for (k, v) in m.pairs() {
                let Cbor::Bytes(v) = v else { return invalid("an extended attribute not bytes") };
                x.xattrs.push((shared.get(name(k)?), shared.get(v)));
            }
        }
        FORKS => {
            let Cbor::Map(m) = v else { return invalid("forks not a map") };
            for (k, v) in m.pairs() {
                let r = match v {
                    Cbor::Array(a) => {
                        let mut a = a.iter();
                        match (a.next(), a.next(), a.next(), a.next()) {
                            (Some(Cbor::Int(o)), Some(Cbor::Int(i)), Some(Cbor::Int(s)), None)
                                if o >= 0 && i >= 0 && s >= 0 =>
                            {
                                Some((o as u64, i as u64, s as u64))
                            }
                            _ => None,
                        }
                    }
                    _ => None,
                };
                let Some(r) = r else { return invalid("a bad fork") };
                forks.push((name(k)?.into_owned(), r));
            }
        }
        WIN_LINK_TYPE => x.win_link_type = Some(uint(2)? as u8),
        WIN_ATTRS => match uint(u32::MAX as u64)? as u32 {
            a if a & !WIN_ATTRS_ALLOWED == 0 => x.win_attrs = Some(a),
            _ => return invalid("win-attrs of bits not allowed"),
        },
        UNIX_MODE => x.mode = Some(uint(0o7777)? as u32),
        UNIX_UID => x.uid = Some(uint(u64::MAX)?),
        UNIX_GID => x.gid = Some(uint(u64::MAX)?),
        UNIX_UNAME => x.uname = Some(shared.get(aqr_text(v)?)),
        UNIX_GNAME => x.gname = Some(shared.get(aqr_text(v)?)),
        MAC_FLAGS => match uint(u32::MAX as u64)? as u32 {
            f if f & !MAC_FLAGS_ALLOWED == 0 => x.mac_flags = Some(f),
            _ => return invalid("mac-flags of bits not allowed"),
        },
        _ => {}
    }
    Ok(())
}

/// The bytes of an `aqr-text`.
fn aqr_text(c: Cbor) -> Decoded<Cow<[u8]>> {
    match c {
        Cbor::Text(b) | Cbor::Bytes(b) => Ok(b),
        Cbor::Tag(TAG_WTF8, b) => match b.first() {
            Cbor::Bytes(b) if wtf8(&b) && std::str::from_utf8(&b).is_err() => Ok(b),
            _ => invalid("a bad WTF-8 text"),
        },
        _ => invalid("a bad text in an entry extra"),
    }
}

/// An `aqr-time`, where it is one this crate reads (else `None`: an extended time).
fn time(t: &Cbor) -> Option<Time> {
    // a base time: in nanoseconds, and its scale
    let base = |t: &Cbor| -> Option<(i128, i128)> {
        let scale = match *t {
            Cbor::Int(_) => 1_000_000_000,
            // (the untagged scale's marker is no tag)
            Cbor::Tag(tag, _) if tag != u64::MAX => TAG_SCALES.iter().find(|s| s.0 == tag)?.1,
            _ => return None,
        };
        let v = match t {
            Cbor::Int(v) => *v,
            Cbor::Tag(_, v) => match v.first() {
                Cbor::Int(v) => v,
                _ => return None,
            },
            _ => return None,
        };
        Some((v.checked_mul(scale)?, scale))
    };
    let (ns, unit, zone) = match t {
        Cbor::Array(a) => {
            let mut items = a.iter();
            let (b, offset) = (items.next()?, items.next());
            if items.next().is_some() {
                return None;
            }
            let (ns, unit) = base(&b)?;
            match offset {
                None => (ns, unit, Zone::Local),
                Some(Cbor::Int(o)) if (-48..=56).contains(&o) && unit != DAY as i128 => {
                    (ns, unit, Zone::Offset(o as i8))
                }
                Some(_) => return None,
            }
        }
        t => {
            let (ns, unit) = base(t)?;
            (ns, unit, Zone::Utc)
        }
    };
    let secs = i64::try_from(ns.div_euclid(1_000_000_000)).ok()?;
    Some(Time { secs, nanos: ns.rem_euclid(1_000_000_000) as u32, zone, unit: unit as u64 })
}

/// A member of a map of the optional metadata, as `members` gives it: its key where it is an
/// integer, and its key and value in CBOR's diagnostic notation (RFC 8949, section 8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    pub key: Option<i128>,
    pub key_text: String,
    pub value: String,
    /// The value's text, where it is an `aqr-text` (of a comment, say).
    pub text: Option<Vec<u8>>,
    /// The value's fields, where it is a map of integers to byte strings (`zip-extras`).
    pub fields: Option<Vec<(i128, Vec<u8>)>>,
}

/// The members of optional metadata `d` (well-formed, as [`extras_given`] read it): those of its
/// top-level map but `entry-extras`, and by entry (from the root, `n` besides it) those of its
/// entry extra (`None`: null, or none given), each as it is encoded. Byte and text strings
/// over `cut` bytes are cut there (`None`: none cut).
#[allow(clippy::type_complexity)]
pub(crate) fn members(d: &[u8], n: usize, cut: Option<usize>) -> Decoded<(Vec<Member>, Vec<Option<Vec<Member>>>)> {
    let (mut top, mut entries) = (Vec::new(), vec![None; n + 1]);
    if d.is_empty() {
        return Ok((top, entries));
    }
    let bad = || super::Failure::Invalid("bad CBOR in the optional metadata".into());
    // a member at `*at` of a map, and the key read as an item
    let member = |at: &mut usize| -> Decoded<(Member, Cbor<'_>)> {
        let k0 = *at;
        let k = item(d, at, 1)?;
        let k1 = *at;
        let v = item(d, at, 1)?;
        let key = if let Cbor::Int(v) = k { Some(v) } else { None };
        let value = diagnostic(&d[k1..*at], cut);
        let fields = match &v {
            Cbor::Map(m) => m
                .pairs()
                .map(|(k, v)| match (k, v) {
                    (Cbor::Int(k), Cbor::Bytes(b)) => Some((k, b.into_owned())),
                    _ => None,
                })
                .collect(),
            _ => None,
        };
        let text = aqr_text(v).ok().map(Cow::into_owned);
        Ok((Member { key, key_text: diagnostic(&d[k0..k1], cut), value, text, fields }, k))
    };
    let mut at = 0;
    let (major, _, arg) = head_of(d, &mut at)?;
    if major != 5 {
        return invalid("optional metadata not a map");
    }
    let top_n = count(d, at, arg)?;
    let mut k = 0;
    while !ended(d, &mut at, top_n, k) {
        k += 1;
        let k0 = at;
        let key = item(d, &mut at, 1)?;
        if key != Cbor::Int(ENTRY_EXTRAS) {
            at = k0;
            top.push(member(&mut at)?.0);
            continue;
        }
        let (major, _, arg) = head_of(d, &mut at)?;
        if major != 4 {
            return invalid("entry-extras not an array");
        }
        let list = count(d, at, arg)?;
        let mut e = 0;
        while !ended(d, &mut at, list, e) {
            let slot = entries.get_mut(e).ok_or_else(bad)?;
            e += 1;
            match head_of(d, &mut at)? {
                (7, 22, _) => {}
                (5, _, arg) => {
                    let m = count(d, at, arg)?;
                    let mut got = Vec::new();
                    while !ended(d, &mut at, m, got.len()) {
                        got.push(member(&mut at)?.0);
                    }
                    *slot = Some(got);
                }
                _ => return invalid("an entry extra not a map"),
            }
        }
    }
    Ok((top, entries))
}

/// The name of member `k` of an entry extra (spec, "Optional metadata").
pub fn entry_key_name(k: i128) -> Option<&'static str> {
    Some(match k {
        CRTIME => "crtime",
        CTIME => "ctime",
        MTIME => "mtime",
        ATIME => "atime",
        ARCTIME => "arctime",
        -6 => "comment",
        LINK_GROUP => "link-group",
        XATTRS => "xattrs",
        FORKS => "forks",
        WIN_LINK_TYPE => "win-link-type",
        WIN_ATTRS => "win-attrs",
        UNIX_MODE => "unix-mode",
        UNIX_UID => "unix-uid",
        UNIX_GID => "unix-gid",
        UNIX_UNAME => "unix-uname",
        UNIX_GNAME => "unix-gname",
        MAC_FLAGS => "mac-flags",
        -100 => "zip-extras",
        _ => return None,
    })
}

/// The name of member `k` of the top-level map of the optional metadata.
pub fn archive_key_name(k: i128) -> Option<&'static str> {
    match k {
        -1 => Some("archive-comment"),
        ENTRY_EXTRAS => Some("entry-extras"),
        SEGMENT_STATUS => Some("segment-status"),
        JOURNAL => Some("journal"),
        _ => None,
    }
}

/// Well-formed CBOR data item `d` in diagnostic notation: indefinite lengths as `_`, strings in
/// chunks as `(_ ...)`, byte strings in hexadecimal; strings over `cut` bytes cut there (`...`
/// and how many bytes there are in all).
pub fn diagnostic(d: &[u8], cut: Option<usize>) -> String {
    let mut out = String::new();
    let mut at = 0;
    if diag_item(d, &mut at, cut, &mut out, 0).is_err() || at != d.len() {
        out += " <bad CBOR>";
    }
    out
}

fn diag_item(d: &[u8], at: &mut usize, cut: Option<usize>, out: &mut String, depth: usize) -> Decoded<()> {
    use std::fmt::Write;
    if depth > CBOR_DEPTH {
        return super::resource("CBOR nested too deep");
    }
    let (major, ai, arg) = head_of(d, at)?;
    let string = |b: &[u8], out: &mut String| {
        let (shown, more) = match cut {
            Some(c) if b.len() > c => (&b[..c], true),
            _ => (b, false),
        };
        if major == 2 {
            out.push_str("h'");
            shown.iter().for_each(|x| _ = write!(out, "{x:02x}"));
            out.push('\'');
        } else {
            let t = String::from_utf8_lossy(shown);
            _ = write!(out, "{t:?}");
        }
        if more {
            _ = write!(out, "...<{} bytes>", b.len());
        }
    };
    match (major, arg) {
        (0, Some(v)) => _ = write!(out, "{v}"),
        (1, Some(v)) => _ = write!(out, "{}", -1 - v as i128),
        (2 | 3, Some(n)) => {
            let n =
                usize::try_from(n).ok().filter(|&n| n <= d.len() - *at).ok_or(super::Failure::Invalid("".into()))?;
            string(&d[*at..*at + n], out);
            *at += n;
        }
        (2 | 3, None) => {
            out.push_str("(_ ");
            let mut first = true;
            while d.get(*at) != Some(&0xff) {
                if !std::mem::take(&mut first) {
                    out.push_str(", ");
                }
                diag_item(d, at, cut, out, depth + 1)?;
            }
            *at += 1;
            out.push(')');
        }
        (4 | 5, arg) => {
            let n = count(d, *at, arg)?;
            out.push(if major == 4 { '[' } else { '{' });
            if n.is_none() {
                out.push_str("_ ");
            }
            let mut k = 0;
            while !ended(d, at, n, k) {
                if k > 0 {
                    out.push_str(", ");
                }
                diag_item(d, at, cut, out, depth + 1)?;
                if major == 5 {
                    out.push_str(": ");
                    diag_item(d, at, cut, out, depth + 1)?;
                }
                k += 1;
            }
            out.push(if major == 4 { ']' } else { '}' });
        }
        (6, Some(tag)) => {
            _ = write!(out, "{tag}(");
            diag_item(d, at, cut, out, depth + 1)?;
            out.push(')');
        }
        (7, Some(v)) => match ai {
            20 => out.push_str("false"),
            21 => out.push_str("true"),
            22 => out.push_str("null"),
            23 => out.push_str("undefined"),
            0..24 => _ = write!(out, "simple({v})"),
            24 => _ = write!(out, "simple({v})"),
            25..28 => {
                let f = match ai {
                    25 => half(v as u16),
                    26 => f32::from_bits(v as u32) as f64,
                    _ => f64::from_bits(v),
                };
                let s = match f {
                    f if f.is_nan() => "NaN".into(),
                    f if f.is_infinite() => if f > 0.0 { "Infinity" } else { "-Infinity" }.into(),
                    f => format!("{f:?}"),
                };
                _ = write!(out, "{s}_{}", ai - 24);
            }
            _ => return invalid(""),
        },
        _ => return invalid(""),
    }
    Ok(())
}

/// An IEEE 754 half-precision float's value.
fn half(h: u16) -> f64 {
    let (sign, exp, frac) = (if h >> 15 != 0 { -1.0 } else { 1.0 }, (h >> 10) & 0x1f, (h & 0x3ff) as f64);
    sign * match exp {
        0 => frac * 2f64.powi(-24),
        31 if frac == 0.0 => f64::INFINITY,
        31 => f64::NAN,
        e => (1.0 + frac / 1024.0) * 2f64.powi(e as i32 - 15),
    }
}

/// A metadata block's internal structure read: the entries (by number) and the files'
/// parameters, with the relative offsets of the module and resource blocks.
pub(crate) struct Metadata {
    pub prev: u64,
    pub modules: Vec<u64>,
    pub resources: Vec<u64>,
    pub dictionaries: Vec<u64>,
    /// Of each entry from 1: its parent's number, its name, and what it is (for a file, the
    /// relative offset of its leaf block, its output, its size and its dictionary).
    pub entries: Vec<(usize, Vec<u8>, Read)>,
    /// From the root, by number: the extras, and the forks as they refer to them.
    pub extras: Vec<Extra>,
    pub forks: Vec<Vec<(Vec<u8>, ForkRef)>>,
    /// From the root, by number: whether each has an entry extra (not null).
    pub given: Vec<bool>,
    /// Its `segment-status`: a hint of repair and of what the writing left out, not of the
    /// archive's value.
    pub status: Status,
    /// Its `journal`.
    pub journal: Journal,
    /// Where the optional metadata is in the block's data.
    pub opt: std::ops::Range<usize>,
}

#[derive(Clone)]
pub(crate) enum Read {
    Dir,
    File {
        offset: u64,
        output: u64,
        size: u64,
        /// 1-based, zero for none.
        dictionary: u64,
    },
    Link(Vec<u8>),
    Fifo,
    Device {
        block: bool,
        major: u64,
        minor: u64,
    },
    /// A special entry this crate does not take (a tombstone or one undefined), by its kind.
    Special(u64),
}

/// Reads the internal structure of a metadata block.
pub(crate) fn read(d: &[u8]) -> Decoded<Metadata> {
    read_with(d, &mut |_| None)
}

/// The entries of a metadata block (from 1: each its parent's number, its name, and what it is),
/// as [`read_with`] tells them.
pub(crate) type Entries = [(usize, Vec<u8>, Read)];

/// [`read`], the extras read of the entries alone that `wanted` tells, told the entries (by
/// number from the root; none: all of them): the others' left default, their forks left out.
pub(crate) fn read_with(d: &[u8], wanted: &mut dyn FnMut(&Entries) -> Option<Vec<bool>>) -> Decoded<Metadata> {
    let mut at = 0;
    let leb = |at: &mut usize| super::leb(d, at);
    let prev = leb(&mut at)?;
    let mut counts = [0usize; 8];
    for c in &mut counts {
        *c = usize::try_from(leb(&mut at)?)
            .ok()
            .filter(|&n| n <= d.len())
            .ok_or_else(|| super::Failure::Invalid("a count out of range".into()))?;
    }
    let [modules, resources, dictionaries, dirs, files, specials, entries_size, opt_size] = counts;
    let n = dirs + files + specials;
    let get_u = |at: &mut usize, bound: u64| -> Decoded<u64> {
        let w = width(bound);
        let b = d.get(*at..*at + w).ok_or_else(|| super::Failure::Invalid("truncated metadata".into()))?;
        *at += w;
        let v = b.iter().rev().fold(0u64, |v, &x| (v << 8) | x as u64);
        if v > bound { invalid("a field out of range") } else { Ok(v) }
    };
    let module_offsets = (0..modules).map(|_| get_u(&mut at, prev)).collect::<Decoded<Vec<_>>>()?;
    let resource_offsets = (0..resources).map(|_| get_u(&mut at, prev)).collect::<Decoded<Vec<_>>>()?;
    let dictionary_offsets = (0..dictionaries).map(|_| get_u(&mut at, prev)).collect::<Decoded<Vec<_>>>()?;
    let mut parents = vec![0];
    for k in 2..=n {
        let p = get_u(&mut at, dirs as u64)? as usize;
        if p >= k {
            return invalid("a parent after its entry");
        }
        parents.push(p);
    }
    let mut params = Vec::new();
    for _ in 0..files {
        let dictionary = get_u(&mut at, dictionaries as u64)?;
        let offset = get_u(&mut at, prev)?;
        let output = match offset % 2 {
            1 => leb(&mut at)?.checked_add(1).ok_or_else(|| super::Failure::Invalid("bad file output".into()))?,
            _ => 0,
        };
        params.push((offset - offset % 2, output, dictionary));
    }
    let offsets = (2..=n).map(|_| get_u(&mut at, entries_size as u64)).collect::<Decoded<Vec<_>>>()?;
    let entries = d.get(at..at + entries_size).ok_or_else(|| super::Failure::Invalid("truncated entries".into()))?;
    at += entries_size;
    let opt = d.get(at..at + opt_size).ok_or_else(|| super::Failure::Invalid("truncated optional metadata".into()))?;
    if at + opt_size != d.len() {
        return invalid("bytes after the optional metadata");
    }

    let mut out = Vec::with_capacity(n);
    let mut p = 0;
    for k in 1..=n {
        if k >= 2 && offsets[k - 2] as usize != p {
            return invalid("an entry offset not where its entry is");
        }
        let end = offsets.get(k - 1).map_or(entries.len(), |&o| o as usize);
        let e = entries.get(p..end).ok_or_else(|| super::Failure::Invalid("bad entry offsets".into()))?;
        let mut q =
            e.iter().position(|&b| b == 0).ok_or_else(|| super::Failure::Invalid("an unended entry name".into()))?;
        let name = e[..q].to_vec();
        if !valid_name(&name) {
            return invalid("an invalid entry name");
        }
        q += 1;
        let read = if k <= dirs {
            Read::Dir
        } else if k <= dirs + files {
            let size = super::leb(e, &mut q)?;
            let (offset, output, dictionary) = params[k - dirs - 1];
            Read::File { offset, output, size, dictionary }
        } else {
            match super::leb(e, &mut q)? {
                1 => {
                    let t = e[q..]
                        .iter()
                        .position(|&b| b == 0)
                        .ok_or_else(|| super::Failure::Invalid("an unended link target".into()))?;
                    let target = e[q..q + t].to_vec();
                    if !valid_target(&target) {
                        return invalid("an invalid link target");
                    }
                    q += t + 1;
                    Read::Link(target)
                }
                0 => Read::Special(0),
                2 => Read::Fifo,
                kind @ 3..=4 => {
                    let major = super::leb(e, &mut q)?;
                    let minor = super::leb(e, &mut q)?;
                    Read::Device { block: kind == 4, major, minor }
                }
                kind => {
                    q = e.len();
                    Read::Special(kind)
                }
            }
        };
        if q != e.len() {
            return invalid("bytes after an entry");
        }
        out.push((parents[k - 1], name, read));
        p = end;
    }
    if p != entries.len() {
        return invalid("bytes after the entries");
    }
    let wanted = wanted(&out);
    let (extras, forks, given, status, journal) = extras_given(opt, n, wanted.as_deref())?;
    Ok(Metadata {
        prev,
        modules: module_offsets,
        resources: resource_offsets,
        dictionaries: dictionary_offsets,
        entries: out,
        extras,
        forks,
        given,
        status,
        journal,
        opt: d.len() - opt_size..d.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Items gone over by their heads ([`skip`]) end where those read do, and are bad where
    /// their heads are.
    #[test]
    fn skipped_as_read() {
        let good: [&[u8]; 9] = [
            &[0xa2, 0x00, 0x01, 0x05, 0xf6],
            &[0xbf, 0x01, 0x9f, 0x02, 0x03, 0xff, 0xff],
            &[0x7f, 0x61, b'a', 0x62, b'b', b'c', 0xff],
            &[0x5f, 0x41, 0x00, 0xff],
            &[0xc1, 0x1a, 0, 0, 0, 1],
            &[0xfa, 0, 0, 0, 0],
            &[0x83, 0x18, 0xff, 0x38, 0x00, 0xf8, 0x20],
            &[0x82, 0xa1, 0x00, 0x80, 0xf5],
            &[0x3b, 0, 0, 0, 0, 0, 0, 0, 1],
        ];
        for d in good {
            let (mut a, mut b) = ([d, &[0xf6]].concat(), 0);
            let mut c = 0;
            item(&a, &mut b, 1).unwrap();
            skip(&a, &mut c, 1).unwrap();
            assert_eq!((b, c), (d.len(), d.len()), "{d:x?}");
            a.truncate(d.len() - 1);
            assert!(skip(&a, &mut 0, 1).is_err(), "{d:x?} cut short");
        }
        // (a pair of an indefinite map cut by a break, chunks of another type, a simple value of
        // two bytes below 32, a reserved head)
        for d in [&[0xbf, 0x00, 0xff][..], &[0x7f, 0x41, 0x00, 0xff], &[0xf8, 0x10], &[0x1c]] {
            assert!(skip(d, &mut 0, 1).is_err() && item(d, &mut 0, 1).is_err(), "{d:x?}");
        }
    }

    #[test]
    fn wtf8_names() {
        assert!(wtf8("añ日🦀".as_bytes()));
        // a lone surrogate, but not a pair of them
        assert!(wtf8(&[0xed, 0xa0, 0x80]));
        assert!(!wtf8(&[0xed, 0xa0, 0x80, 0xed, 0xb0, 0x80]));
        assert!(!wtf8(&[0xc0, 0x80]) && !wtf8(&[0xff]) && !wtf8(&[0xe2, 0x82]));
        assert_eq!(name_from(b"a\xffb"), b"\xc1a\xffb");
        assert_eq!(name_bytes(b"\xc1a\xffb"), (&b"a\xffb"[..], true));
        assert!(
            !valid_name(b"..")
                && !valid_name(b"a/b")
                && !valid_name(b"")
                && !valid_name(b"\xc1..")
                && valid_name(b"\xc1\xff")
        );
    }

    #[test]
    fn times_of_zones_and_units() {
        let read = |d: &[u8]| -> Option<Time> {
            let mut at = 0;
            let c = item(d, &mut at, 0).unwrap();
            assert_eq!(at, d.len());
            time(&c)
        };
        let day = Time { unit: DAY, ..Time::new(86400 * 3, 0) };
        let cases: [(&[u8], Option<Time>); 7] = [
            // 100(3): a date
            (&[0xd8, 100, 0x03], Some(day)),
            // [100(3)]: a local date
            (&[0x81, 0xd8, 100, 0x03], Some(Time { zone: Zone::Local, ..day })),
            // [121(5), 36]: a point in time recorded at UTC+9, of 2 seconds
            (
                &[0x82, 0xd8, 121, 0x05, 0x18, 36],
                Some(Time { zone: Zone::Offset(36), unit: 2_000_000_000, ..Time::new(10, 0) }),
            ),
            // [7, -48]: of seconds, at UTC-12
            (&[0x82, 0x07, 0x38, 47], Some(Time { zone: Zone::Offset(-48), unit: 1_000_000_000, ..Time::new(7, 0) })),
            // [100(3), 4]: a date has no offset; nor is 57 one
            (&[0x82, 0xd8, 100, 0x03, 0x04], None),
            (&[0x82, 0x07, 0x18, 57], None),
            // 18446744073709551615(5): a tag of no time, not the seconds of an untagged one
            (&[0xdb, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x05], None),
        ];
        for (d, want) in cases {
            assert_eq!(read(d), want, "{d:02x?}");
            if let Some(t) = want {
                let mut out = Vec::new();
                time_of(&mut out, t);
                assert_eq!(out, d, "{t:?}");
            }
        }
        assert_eq!(Time::new(0, 0).civil(), (1970, 1, 1, 0, 0, 0));
        assert_eq!(Time::new(951_825_599, 0).civil(), (2000, 2, 29, 11, 59, 59));
        assert_eq!(Time { zone: Zone::Offset(-4), ..Time::new(0, 0) }.civil(), (1969, 12, 31, 23, 0, 0));
        assert_eq!(Time::new(-62_135_596_801, 0).civil(), (0, 12, 31, 23, 59, 59));
        let x = Extra { mtime: Some(Time { zone: Zone::Local, ..day }), atime: Some(day), ..Extra::default() };
        assert_eq!(*x.instants(), Extra { atime: Some(day), ..Extra::default() });
    }

    #[test]
    fn optional_metadata() {
        let mut t = Tree::default();
        let x = Extra { mtime: Some(Time::new(-5, 7)), mode: Some(0o755), ..Extra::default() };
        let y = Extra { mtime: Some(Time::new(1 << 40, 0)), forks: vec![(b"f".to_vec(), 2)], ..x.clone() };
        for (i, extra) in [Extra::default(), x, y].into_iter().enumerate() {
            let node = EntryNode::File(i);
            t.entries.push(Entry { parent: None, name: vec![b'a' + i as u8], node, extra });
        }
        let order: Vec<usize> = (0..3).collect();
        let c = optional(&t, &order, &|c| [c as u64 * 2, 0, 9], Status::None, &[]);
        let (got, forks) = extras(&c, 3).unwrap();
        assert_eq!(got[1..3], [t.entries[0].extra.clone(), t.entries[1].extra.clone()]);
        // (a time beyond 64 bits of nanoseconds of seconds)
        let seconds = Some(Time { unit: 1_000_000_000, ..Time::new(1 << 40, 0) });
        assert_eq!(got[3], Extra { forks: Vec::new(), mtime: seconds, ..t.entries[2].extra.clone() });
        assert_eq!(forks[3], [(b"f".to_vec(), (4, 0, 9))]);
        // (times of nanoseconds as they are)
        assert!(c.windows(2).any(|w| w == [0xd8, 126]));
        let none = Tree { entries: t.entries[..1].to_vec(), ..Tree::default() };
        assert_eq!(optional(&none, &[0], &|_| unreachable!(), Status::None, &[]), b"");
        // segment-status, alone and after entry-extras: true, or a map of what was left out
        assert_eq!(optional(&none, &[0], &|_| unreachable!(), Status::Complete, &[]), [0xa1, 0x22, 0xf5]);
        assert_eq!(extras_given(&[0xa1, 0x22, 0xf5], 1, None).unwrap().3, Status::Complete);
        let both = optional(&t, &order, &|c| [c as u64 * 2, 0, 9], Status::Complete, &[]);
        let (again, _, _, status, _) = extras_given(&both, 3, None).unwrap();
        assert!(status == Status::Complete && again == extras(&c, 3).unwrap().0);
        assert_eq!(extras_given(&c, 3, None).unwrap().3, Status::None);
        assert!(extras_given(&[0xa1, 0x22, 0xf4], 1, None).is_err());
        let left = Status::Incomplete { files: Some(3), bytes: Some(300) };
        let m = optional(&none, &[0], &|_| unreachable!(), left, &[]);
        assert_eq!(m, [0xa1, 0x22, 0xa2, 0x00, 0x03, 0x01, 0x19, 0x01, 0x2c]);
        assert_eq!(extras_given(&m, 1, None).unwrap().3, left);
        let bare = Status::Incomplete { files: None, bytes: None };
        assert_eq!(optional(&none, &[0], &|_| unreachable!(), bare, &[]), [0xa1, 0x22, 0xa0]);
        assert_eq!(extras_given(&[0xa1, 0x22, 0xa0], 1, None).unwrap().3, bare);
        // (other keys let be; a count of another kind, or the same key twice, not)
        let other = Status::Incomplete { files: Some(1), bytes: None };
        assert_eq!(extras_given(&[0xa1, 0x22, 0xa2, 0x00, 0x01, 0x05, 0xf6], 1, None).unwrap().3, other);
        assert!(extras_given(&[0xa1, 0x22, 0xa1, 0x00, 0x20], 1, None).is_err());
        // journal: times to labels, written in the order of the times
        let points = vec![(1_700_000_000_000, "daily".to_string()), (5, String::new())];
        let j = optional(&none, &[0], &|_| unreachable!(), Status::Complete, &points);
        assert_eq!(&j[..5], [0xa2, 0x22, 0xf5, 0x23, 0xa2]);
        let got = extras_given(&j, 1, None).unwrap();
        assert_eq!(
            (got.3, &got.4[..]),
            (Status::Complete, &[(5, String::new()), (1_700_000_000_000, "daily".to_string())][..])
        );
        assert!(extras_given(&[0xa1, 0x23, 0xa2, 0x05, 0x60, 0x05, 0x60], 1, None).is_err());
        assert!(extras_given(&[0xa1, 0x23, 0xa1, 0x20, 0x60], 1, None).is_err());
        assert!(extras_given(&[0xa1, 0x23, 0x80], 1, None).is_err());
        assert!(extras_given(&[0xa1, 0x22, 0xa2, 0x00, 0x01, 0x00, 0x02], 1, None).is_err());
        // other keys, tags and indefinite lengths read
        let other = [0xbf, 0x21, 0x9f, 0xa1, 0x22, 0xd8, 0x79, 0x0a, 0xf6, 0xff, 0x01, 0x5f, 0x41, 0x00, 0xff, 0xff];
        let (got, _) = extras(&other, 2).unwrap();
        assert_eq!(got[0].mtime, Some(Time { unit: 2_000_000_000, ..Time::new(20, 0) }));
        // texts of each kind, and bad ones
        let names = [0xa1, 0x21, 0x81, 0xa1, 0x29, 0xa3, 0x61, b'a', 0x40, 0x41, 0xff, 0x40, 0xd9, 0x01, 0x11];
        let (got, _) = extras(&[&names[..], &[0x43, 0xed, 0xa0, 0x80, 0x40]].concat(), 1).unwrap();
        let want: [Xattr; 3] = [
            (b"a"[..].into(), b""[..].into()),
            (b"\xff"[..].into(), b""[..].into()),
            (b"\xed\xa0\x80"[..].into(), b""[..].into()),
        ];
        assert_eq!(got[0].xattrs, want);
        assert!(extras(&[&names[..], &[0x41, b'a', 0x40]].concat(), 1).is_err());
        assert!(extras(&[0xa1, 0x21, 0x81, 0xa1, 0x38, 0x4f, 0x18, 0x20], 1).is_err());
        assert!(extras(&[0xa2, 0x01, 0x00, 0x01, 0x00], 1).is_err());
        // keys of other contents not duplicates
        assert!(extras(&[0xa2, 0x61, b'a', 0x00, 0x61, b'b', 0x00], 1).is_ok());
        assert!(extras(&[0xa2, 0xf9, 0x3c, 0x00, 0x00, 0xf9, 0x40, 0x00, 0x00], 1).is_ok());
        assert!(extras(&[0xa1, 0x21, 0x83, 0xf6, 0xf6, 0xf6], 1).is_err());
    }

    #[test]
    fn members_as_encoded() {
        // {-1: "c", 5: h'00ff', -2: [null, {-3: 10, -999: [_ 1, 2.5]}]}
        let d = [
            0xa3, 0x20, 0x61, b'c', 0x05, 0x42, 0x00, 0xff, 0x21, 0x82, 0xf6, 0xa2, 0x22, 0x0a, 0x39, 0x03, 0xe6, 0x9f,
            0x01, 0xf9, 0x41, 0x00, 0xff,
        ];
        let (got, _) = extras(&d, 1).unwrap();
        assert_eq!(got[1].mtime, Some(Time { unit: 1_000_000_000, ..Time::new(10, 0) }));
        let (top, entries) = members(&d, 1, None).unwrap();
        let shown = |m: &[Member]| m.iter().map(|m| (m.key, m.value.clone())).collect::<Vec<_>>();
        assert_eq!(shown(&top), [(Some(-1), "\"c\"".into()), (Some(5), "h'00ff'".into())]);
        assert_eq!(top[0].text.as_deref(), Some(&b"c"[..]));
        assert_eq!(entries[0], None);
        let e = entries[1].as_ref().unwrap();
        assert_eq!(shown(e), [(Some(-3), "10".into()), (Some(-999), "[_ 1, 2.5_1]".into())]);
        assert_eq!(
            (entry_key_name(-3), entry_key_name(-999), archive_key_name(-1)),
            (Some("mtime"), None, Some("archive-comment"))
        );
        // strings cut, chunks and other items
        assert_eq!(diagnostic(&[0x45, 1, 2, 3, 4, 5], Some(2)), "h'0102'...<5 bytes>");
        assert_eq!(diagnostic(&[0x7f, 0x61, b'a', 0x61, b'b', 0xff], None), "(_ \"a\", \"b\")");
        assert_eq!(diagnostic(&[0xbf, 0xf4, 0xf5, 0xff], None), "{_ false: true}");
        assert_eq!(diagnostic(&[0xfa, 0x7f, 0xc0, 0, 0], None), "NaN_2");
    }

    #[test]
    fn optional_metadata_keys() {
        // keys of maps and arrays, and tagged ones, the same as items (of either length)
        let map = |pairs: &[&[u8]]| [&[0xa1u8, 0x01, 0xa0 + pairs.len() as u8 / 2][..], &pairs.concat()].concat();
        let (one, two, two_) = (&[0x82, 0x01, 0x02][..], &[0x82, 0x01, 0x03][..], &[0x9f, 0x01, 0x03, 0xff][..]);
        assert!(extras(&map(&[one, &[0], two, &[0]]), 0).is_ok());
        assert!(extras(&map(&[two, &[0], two_, &[0]]), 0).is_err());
        assert!(extras(&map(&[&[0xc1, 0x01], &[0], &[0xc1, 0x18, 0x01], &[0]]), 0).is_err());
        assert!(extras(&map(&[&[0xc1, 0x01], &[0], &[0xc2, 0x01], &[0]]), 0).is_ok());
        assert!(extras(&map(&[&[0xa1, 0x01, 0x02], &[0], &[0xbf, 0x01, 0x02, 0xff], &[0]]), 0).is_err());
        // of a map of more keys than are kept at once, the last the same as the first
        let mut many = vec![0xa1, 0x01, 0xb8, 20];
        (0..20u8).for_each(|k| many.extend([k, 0]));
        assert!(extras(&many, 0).is_ok());
        *many.iter_mut().rev().nth(1).unwrap() = 0;
        assert!(extras(&many, 0).is_err());
    }

    #[test]
    fn optional_metadata_big_maps() {
        // maps of many keys (a map's value, the top map, an entry extra) read in time, their
        // duplicates found
        const N: u32 = 100_000;
        let map = |n: u32, f: &dyn Fn(u32) -> u32| {
            let mut m = vec![0xba];
            m.extend(n.to_be_bytes());
            (0..n).for_each(|k| {
                m.push(0x3a);
                m.extend((f(k) + 100).to_be_bytes());
                m.push(0);
            });
            m
        };
        // (in a map's value, as the top map, as an entry extra)
        let case = |c: usize, f: &dyn Fn(u32) -> u32| match c {
            0 => [&[0xa1, 0x01][..], &map(N, f)].concat(),
            1 => map(N, f),
            _ => [&[0xa1, 0x21, 0x81][..], &map(N, f)].concat(),
        };
        for c in 0..3 {
            let start = std::time::Instant::now();
            assert!(extras(&case(c, &|k| k), 0).is_ok());
            assert!(extras(&case(c, &|k| if k == N - 1 { 0 } else { k }), 0).is_err());
            assert!(start.elapsed().as_secs_f64() < 2.0, "{:?}", start.elapsed());
        }
    }

    #[test]
    fn optional_metadata_shared() {
        // the same texts of several entries taken once
        let mut t = Tree::default();
        let x = Extra {
            uname: Some(b"me"[..].into()),
            xattrs: vec![(b"user.a"[..].into(), b"1"[..].into())],
            ..Extra::default()
        };
        for i in 0..3 {
            let extra = Extra { gname: Some(vec![b'g', i % 2].into()), ..x.clone() };
            t.entries.push(Entry { parent: None, name: vec![b'a' + i], node: EntryNode::Dir, extra });
        }
        let c = optional(&t, &[0, 1, 2], &|_| unreachable!(), Status::None, &[]);
        let (got, _) = extras(&c, 3).unwrap();
        assert_eq!(got[1..], t.entries.iter().map(|e| e.extra.clone()).collect::<Vec<_>>()[..]);
        let same = |f: &dyn Fn(&Extra) -> &Arc<[u8]>| got[1..].windows(2).all(|w| Arc::ptr_eq(f(&w[0]), f(&w[1])));
        assert!(same(&|x| x.uname.as_ref().unwrap()) && same(&|x| &x.xattrs[0].0) && same(&|x| &x.xattrs[0].1));
        assert!(Arc::ptr_eq(got[1].gname.as_ref().unwrap(), got[3].gname.as_ref().unwrap()));
    }

    /// Trees of segments merged: tombstones taking away a file and a directory with all under it
    /// (and nothing where nothing has the name, even under a directory new), a directory replaced
    /// by a link and then made again (empty), the root's extra kept where a segment has none, and
    /// the contents of the files left alone kept.
    #[test]
    fn merged() {
        let e =
            |parent, name: &str, node| Entry { parent, name: name.as_bytes().to_vec(), node, extra: Extra::default() };
        let seg = |entries: Vec<Entry>, root: Option<u32>, refs: usize, tombstones: Vec<(Option<usize>, &str)>| {
            let given = std::iter::once(root.is_some()).chain(entries.iter().map(|_| false)).collect();
            SegmentTree {
                tree: Tree { root: Extra { mode: root, ..Extra::default() }, entries },
                refs: (0..refs).map(|r| (r, 0, r as u64)).collect(),
                given,
                tombstones: tombstones.into_iter().map(|(p, n)| (p, n.as_bytes().to_vec())).collect(),
            }
        };
        let (t, refs) = merge(vec![
            seg(
                vec![
                    e(None, "d", EntryNode::Dir),
                    e(Some(0), "a", EntryNode::File(0)),
                    e(Some(0), "s", EntryNode::Dir),
                    e(Some(2), "b", EntryNode::File(1)),
                    e(None, "l", EntryNode::Dir),
                    e(Some(4), "c", EntryNode::File(2)),
                    e(None, "f", EntryNode::File(3)),
                ],
                Some(0o700),
                4,
                vec![],
            ),
            seg(
                vec![
                    e(None, "d", EntryNode::Dir),
                    e(None, "l", EntryNode::Link(b"x".to_vec())),
                    e(None, "n", EntryNode::Dir),
                ],
                None,
                0,
                vec![(Some(0), "s"), (None, "f"), (None, "none"), (Some(2), "none")],
            ),
            seg(vec![e(None, "l", EntryNode::Dir), e(Some(0), "g", EntryNode::File(0))], None, 1, vec![]),
        ]);
        let paths: Vec<(String, &EntryNode)> = (t.entries.iter())
            .map(|x| {
                let mut p = String::from_utf8(x.name.clone()).unwrap();
                let mut up = x.parent;
                while let Some(u) = up {
                    p = format!("{}/{p}", String::from_utf8(t.entries[u].name.clone()).unwrap());
                    up = t.entries[u].parent;
                }
                (p, &x.node)
            })
            .collect();
        let want: [(&str, EntryNode); 5] = [
            ("d", EntryNode::Dir),
            ("d/a", EntryNode::File(0)),
            ("n", EntryNode::Dir),
            ("l", EntryNode::Dir),
            ("l/g", EntryNode::File(1)),
        ];
        assert_eq!(paths, want.iter().map(|(p, n)| (p.to_string(), n)).collect::<Vec<_>>());
        assert_eq!(t.root.mode, Some(0o700));
        // (of the first segment's first content, and the third's)
        assert_eq!(refs, [(0, 0, 0), (0, 0, 0)]);
    }

    /// The segments of [`merged`] resolved: what became of each entry not left, and where the
    /// entries left (and the extras of the directories) are of.
    #[test]
    fn resolved() {
        let sh = |parent, name: &'static str, kind| Shape { parent, name: name.as_bytes(), kind, given: false };
        let (dir, other, gone) = (Kind::Dir, Kind::Other, Kind::Tombstone);
        let segments = vec![
            vec![
                sh(None, "d", dir),
                sh(Some(0), "a", other),
                sh(Some(0), "s", dir),
                sh(Some(2), "b", other),
                sh(None, "l", dir),
                sh(Some(4), "c", other),
                sh(None, "f", other),
            ],
            vec![
                sh(None, "d", dir),
                sh(None, "l", other),
                sh(None, "n", dir),
                sh(Some(0), "s", gone),
                sh(None, "f", gone),
                sh(None, "none", gone),
                sh(Some(2), "none", gone),
            ],
            vec![sh(None, "l", dir), sh(Some(0), "g", other)],
        ];
        let o = |segment, entry| Origin { segment, entry };
        let mut fates = HashMap::new();
        let r = resolve(&[true, false, false], &segments, &mut |x, f| assert!(fates.insert(x, f).is_none()));
        let want = [
            (o(0, 0), Fate::Merged(o(1, 0))),
            (o(0, 2), Fate::Removed(o(1, 3))),
            (o(0, 3), Fate::WithAncestor(o(0, 2))),
            (o(0, 4), Fate::Replaced(o(1, 1))),
            (o(0, 5), Fate::WithAncestor(o(0, 4))),
            (o(0, 6), Fate::Removed(o(1, 4))),
            (o(1, 1), Fate::Replaced(o(2, 0))),
        ];
        assert_eq!(fates, HashMap::from(want));
        assert_eq!(r.root, Some(0));
        let got: Vec<_> = r.entries.iter().map(|e| (e.parent, e.node, e.extra)).collect();
        assert_eq!(
            got,
            [
                (None, o(1, 0), o(0, 0)),
                (Some(0), o(0, 1), o(0, 1)),
                (None, o(1, 2), o(1, 2)),
                (None, o(2, 0), o(2, 0)),
                (Some(3), o(2, 1), o(2, 1)),
            ]
        );
    }
}
