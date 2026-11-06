//! An archive read for all it records ([`inspect`]), as `aqr -I` shows it: its header and
//! footer, every block, and each segment by itself (spec, "Segments and entries"): its module,
//! resource, dictionary and leaf blocks, its metadata block, and each entry with every member
//! of its entry extra, unknown ones too ([`Member`]); the formula making a content in full
//! ([`Inspection::formulas`]).
//!
//! Unlike [`super::open`], it reads each segment of an archive by itself, every entry of each
//! (tombstones too), without merging them; and overlay archives.
//! What does not hold of the references between the blocks and the metadata blocks is told in
//! [`Segment::problems`] rather than failing.

use std::collections::HashSet;
use std::ops::Range;

use super::listing::{graph, module_names};
use super::tree::{self, Kind, Member, Shape};
pub use super::tree::{Fate, Origin};
use super::{
    Blocks, Decoded, Extra, FOOTER_MAGIC, Failure, HEADER_MAGIC, LeafBlock, OVERLAY_FOOTER_MAGIC, OVERLAY_HEADER_MAGIC,
    critical, frame, invalid, leb, parse,
};

/// An archive inspected.
pub struct Inspection<'c> {
    /// Whether it is an overlay archive (else standalone).
    pub overlay: bool,
    /// The footer's fields: the size hint and the metadata backward offset.
    pub size_hint: u64,
    pub metadata_back: u64,
    /// The bytes of the footer.
    pub footer: usize,
    pub blocks: Vec<Block>,
    pub segments: Vec<Segment>,
    /// Each segment's blocks parsed.
    parsed: Vec<Blocks<'c>>,
    /// The names of the modules, by segment and module block.
    names: Vec<Vec<String>>,
}

/// A block of the archive.
#[derive(Debug, Clone)]
pub struct Block {
    /// Where it starts in the archive.
    pub at: usize,
    pub ty: u64,
    /// The bytes it takes (its type, size and padding too), and those of its data.
    pub size: u64,
    pub data: usize,
    /// The segment it is in (by index); `None` for one outside any (mirror, spacer and
    /// ignored blocks).
    pub segment: Option<usize>,
    /// What it is.
    pub kind: &'static str,
}

/// A segment of the archive: the blocks from the end of the metadata block before it to the end
/// of its own (all of them where the archive has none).
#[derive(Debug, Clone, Default)]
pub struct Segment {
    /// Its blocks, by index in [`Inspection::blocks`].
    pub blocks: Range<usize>,
    pub metadata: Option<Metadata>,
    pub modules: Vec<Aux>,
    pub resources: Vec<Aux>,
    pub dictionaries: Vec<Aux>,
    pub leaves: Vec<Leaf>,
    /// The non-leaf blocks, by where each starts.
    pub non_leaf: Vec<usize>,
    /// The entries by number, the root first (none without a metadata block).
    pub entries: Vec<Entry>,
    /// The members of the top-level map of the optional metadata but `entry-extras`.
    pub archive_extra: Vec<Member>,
    /// What does not hold of the references between its blocks.
    pub problems: Vec<String>,
}

/// A segment's metadata block.
#[derive(Debug, Clone)]
pub struct Metadata {
    pub at: usize,
    /// The bytes of its data decoded, and whether they are stored as Zstandard data.
    pub data: usize,
    pub zstd: bool,
    /// The previous metadata offset, and where it points (the byte before the end of the
    /// header magic, for the first).
    pub prev: u64,
    pub prev_at: Option<usize>,
    /// Directories, files and special entries (tombstones among them).
    pub counts: [usize; 3],
    /// The bytes of the optional metadata.
    pub optional: usize,
}

/// A module, resource or dictionary block.
#[derive(Debug, Clone)]
pub struct Aux {
    pub at: usize,
    pub size: u64,
    /// The bytes decoded (none told of a dictionary).
    pub decoded: Option<usize>,
    /// A module's name, where it is known.
    pub name: Option<String>,
}

/// A leaf block.
#[derive(Debug, Clone)]
pub struct Leaf {
    pub at: usize,
    pub size: u64,
    pub outputs: u64,
    /// A formula block (else a leaf data block).
    pub formula: bool,
    /// The dictionary it takes (by index).
    pub dictionary: Option<usize>,
}

/// An entry of a segment's metadata block.
#[derive(Debug, Clone)]
pub struct Entry {
    /// Its number (the root 0) and its parent's.
    pub number: usize,
    pub parent: usize,
    /// The names from the root, as an entry name is encoded.
    pub names: Vec<Vec<u8>>,
    pub node: Node,
    /// What this crate reads of its entry extra (its forks apart).
    pub extra: Extra,
    /// Its entry extra's members as they are (`None`: null, or none).
    pub members: Option<Vec<Member>>,
    pub forks: Vec<(Vec<u8>, Content)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Node {
    Root,
    Dir,
    File(Content),
    /// A symbolic link, its target encoded as a name is.
    Link(Vec<u8>),
    Fifo,
    Device {
        block: bool,
        major: u64,
        minor: u64,
    },
    Tombstone,
    /// Of a kind undefined.
    Special(u64),
}

/// What an entry of a segment is in the archive as it is, segments merged
/// ([`Inspection::merged`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// In the tree as it is: of the entry whose entry extra it has, where one has (of a
    /// directory, the root among them, that may be one of an earlier segment).
    Current { extra: Option<Origin> },
    /// A tombstone: never in the tree.
    Tombstone,
    /// Not in the tree, and why.
    Gone(Fate),
}

/// A content of a segment: where the metadata block puts it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Content {
    /// Its leaf block (by index in [`Segment::leaves`]; `None` where none is at the offset).
    pub leaf: Option<usize>,
    /// The relative offset the metadata block tells.
    pub offset: u64,
    pub output: u64,
    pub size: u64,
    /// The dictionary the file parameters tell (by index; none of a fork).
    pub dictionary: Option<usize>,
}

/// Archive `c` (the payload of an encrypted archive where `sealed`) inspected, the modules named
/// by `name` (`None`: not known), the byte and text strings of the unknown members shown cut at
/// `cut` bytes (`None`: whole).
pub fn inspect<'c>(
    c: &'c [u8],
    sealed: bool,
    name: &dyn Fn(&[u8]) -> Option<String>,
    cut: Option<usize>,
) -> Decoded<Inspection<'c>> {
    let overlay = c.starts_with(&OVERLAY_HEADER_MAGIC);
    if !overlay && !c.starts_with(&HEADER_MAGIC) {
        return invalid("bad header magic");
    }
    let footer_magic = if overlay { OVERLAY_FOOTER_MAGIC } else { FOOTER_MAGIC };
    let u16_at = |at: usize| u16::from_le_bytes([c[at], c[at + 1]]) as usize;
    let (fields_start, archive_end) = c
        .len()
        .checked_sub(2)
        .and_then(|copy| {
            let fields = copy.checked_sub(u16_at(copy))?;
            Some((fields, fields.checked_sub(8).filter(|&end| end >= HEADER_MAGIC.len())?))
        })
        .ok_or_else(|| Failure::Invalid("truncated footer".into()))?;
    if c[archive_end..archive_end + 6] != footer_magic || u16_at(archive_end + 6) != u16_at(c.len() - 2) {
        return invalid("bad footer");
    }
    let fields = &c[fields_start..c.len() - 2];
    let mut at = 0;
    let (size_hint, metadata_back) = (leb(fields, &mut at)?, leb(fields, &mut at)?);
    if at != fields.len() || size_hint != (archive_end - HEADER_MAGIC.len()) as u64 {
        return invalid("bad footer");
    }
    let d = &c[HEADER_MAGIC.len()..archive_end];
    let framed = frame(d)?;

    // the segments: each up to a metadata block (all of the blocks where there is none)
    let is_metadata = |ty: u64| (0x10..=0x17).contains(&ty);
    let mut cuts: Vec<(usize, usize)> = Vec::new();
    let mut first = 0;
    for (i, &(_, ty, ..)) in framed.iter().enumerate() {
        if is_metadata(ty) {
            cuts.push((first, i + 1));
            first = i + 1;
        }
    }
    let has_metadata = !cuts.is_empty();
    if !has_metadata {
        cuts.push((0, framed.len()));
    }
    // (past the last metadata block, blocks of no segment: content blocks there are invalid)
    let content = |ty: u64| matches!(ty, 0x20..=0x37 | 0x40..=0x4b | 0x50..=0x5f | 0x60..=0x67 | 0x70..=0x77);
    if has_metadata && framed[first..].iter().any(|b| content(b.1)) {
        return invalid("content blocks after the last metadata block");
    }
    let mut blocks: Vec<Block> = framed
        .iter()
        .map(|&(start, ty, data, size)| Block {
            at: start + HEADER_MAGIC.len(),
            ty,
            size,
            data: data.len(),
            segment: None,
            kind: kind_of(ty),
        })
        .collect();
    for (k, &(a, b)) in cuts.iter().enumerate() {
        for bl in &mut blocks[a..b] {
            if content(bl.ty) || is_metadata(bl.ty) {
                bl.segment = Some(k);
            }
        }
    }
    let (mut segments, mut parsed, mut names) = (Vec::new(), Vec::new(), Vec::new());
    let mut prev_meta: Option<usize> = None;
    for &(a, b) in &cuts {
        let start = framed.get(a).map_or(d.len(), |f| f.0);
        let end = framed.get(b).map_or(d.len(), |f| f.0);
        let mut p = parse(&d[start..end], start, size_hint, has_metadata, sealed && start == 0)?;
        let mut seg = Segment { blocks: a..b, ..Segment::default() };
        let sizes = &p.sizes;
        // (by their names alone in the formulas, their versions apart)
        let mods = module_names(&p, &|w| name(w).map(|n| n.split('@').next().unwrap_or(&n).to_owned()));
        seg.modules = (p.modules.iter().enumerate())
            .map(|(i, w)| Aux { at: p.module_at[i], size: sizes.modules[i], decoded: Some(w.len()), name: name(w) })
            .collect();
        seg.resources = (p.resources.iter().enumerate())
            .map(|(i, r)| Aux { at: p.resource_at[i], size: sizes.resources[i], decoded: Some(r.len()), name: None })
            .collect();
        seg.dictionaries = (0..p.dictionaries.len())
            .map(|i| Aux { at: p.dictionary_at[i], size: sizes.dictionaries[i], decoded: None, name: None })
            .collect();
        seg.leaves = (p.leaves.iter().enumerate())
            .map(|(i, l)| Leaf {
                at: p.leaf_at[i],
                size: sizes.leaves[i],
                outputs: match l {
                    LeafBlock::Formula { outputs, .. } => *outputs,
                    _ => 1,
                },
                formula: matches!(l, LeafBlock::Formula { .. }),
                dictionary: p.leaf_dictionary[i],
            })
            .collect();
        seg.non_leaf = p.non_leaf_at.clone();
        if let Some((meta_at, data)) = p.segments.last_mut().and_then(|s| s.metadata.take()) {
            let ty = framed[b - 1].1;
            let prev_at = prev_meta.unwrap_or(HEADER_MAGIC.len() - 1);
            read_metadata(&mut seg, &p, meta_at, ty, &data, prev_at, cut)?;
            prev_meta = Some(meta_at);
        }
        segments.push(seg);
        parsed.push(p);
        names.push(mods);
    }
    if has_metadata {
        let last = segments.last().and_then(|s| s.metadata.as_ref()).map(|m| m.at);
        let back = usize::try_from(metadata_back).ok().and_then(|b| (fields_start).checked_sub(b));
        if back != last {
            return invalid("no metadata block where the footer points");
        }
    } else if metadata_back != 0 {
        return invalid("a metadata block in the footer, but none");
    }
    Ok(Inspection { overlay, size_hint, metadata_back, footer: c.len() - archive_end, blocks, segments, parsed, names })
}

/// The entries of the metadata block at `meta_at` of type `ty` (of data `data`) into `seg`, of
/// blocks `p`; its previous metadata offset to point to `prev_at`.
fn read_metadata(
    seg: &mut Segment,
    p: &Blocks,
    meta_at: usize,
    ty: u64,
    data: &[u8],
    prev_at: usize,
    cut: Option<usize>,
) -> Decoded<()> {
    let m = tree::read(data)?;
    let points = usize::try_from(m.prev).ok().and_then(|o| meta_at.checked_sub(o));
    if points != Some(prev_at) {
        seg.problems.push(format!("its previous metadata offset {} not to {prev_at}", m.prev));
    }
    let n = m.entries.len();
    let mut counts = [0; 3];
    for (_, _, r) in &m.entries {
        counts[match r {
            tree::Read::Dir => 0,
            tree::Read::File { .. } => 1,
            _ => 2,
        }] += 1;
    }
    seg.metadata = Some(Metadata {
        at: meta_at,
        data: data.len(),
        zstd: ty & 0x04 != 0,
        prev: m.prev,
        prev_at: points,
        counts,
        optional: m.opt.len(),
    });
    for (what, offsets, at) in [
        ("module", &m.modules, &p.module_at),
        ("resource", &m.resources, &p.resource_at),
        ("dictionary", &m.dictionaries, &p.dictionary_at),
    ] {
        let told: Vec<Option<usize>> = offsets.iter().map(|&o| meta_at.checked_sub(o as usize)).collect();
        if told.len() != at.len() || told.iter().zip(at).any(|(t, &b)| *t != Some(b)) {
            seg.problems.push(format!("its {what} offsets not those of the {what} blocks"));
        }
    }
    let content = |seg: &mut Segment, offset: u64, output: u64, size: u64, dictionary: Option<usize>| {
        let leaf = meta_at.checked_sub(offset as usize).and_then(|b| p.leaf_at.binary_search(&b).ok());
        match leaf {
            None => seg.problems.push(format!("no leaf block at relative offset {offset}")),
            Some(l) if output >= seg.leaves[l].outputs => {
                seg.problems.push(format!("no output {output} of the leaf block at {}", p.leaf_at[l]))
            }
            Some(_) => {}
        }
        Content { leaf, offset, output, size, dictionary }
    };
    let (archive_extra, mut members) = tree::members(&data[m.opt.clone()], n, cut)?;
    seg.archive_extra = archive_extra;
    let mut extras = m.extras;
    let mut forks = m.forks;
    let mut paths: Vec<Vec<Vec<u8>>> = vec![Vec::new()];
    let mut entry = |seg: &mut Segment, number: usize, parent: usize, node: Node, names: Vec<Vec<u8>>| {
        let forks = std::mem::take(&mut forks[number])
            .into_iter()
            .map(|(name, (o, out, size))| (name, content(seg, o, out, size, None)))
            .collect();
        seg.entries.push(Entry {
            number,
            parent,
            names,
            node,
            extra: std::mem::take(&mut extras[number]),
            members: members[number].take(),
            forks,
        });
    };
    entry(seg, 0, 0, Node::Root, Vec::new());
    for (k, (parent, name, read)) in m.entries.into_iter().enumerate() {
        let k = k + 1;
        let mut names = paths[parent].clone();
        names.push(name);
        let node = match read {
            tree::Read::Dir => Node::Dir,
            tree::Read::File { offset, output, size, dictionary } => {
                let dict = dictionary.checked_sub(1).map(|d| d as usize);
                let c = content(seg, offset, output, size, dict);
                if let Some(l) = c.leaf
                    && seg.leaves[l].dictionary != dict
                {
                    seg.problems.push(format!("entry #{k}'s dictionary not that of its leaf block"));
                }
                Node::File(c)
            }
            tree::Read::Link(t) => Node::Link(t),
            tree::Read::Fifo => Node::Fifo,
            tree::Read::Device { block, major, minor } => Node::Device { block, major, minor },
            tree::Read::Special(0) => Node::Tombstone,
            tree::Read::Special(kind) => Node::Special(kind),
        };
        paths.push(names.clone());
        entry(seg, k, parent, node, names);
    }
    Ok(())
}

/// What a block of type `ty` is (spec, "Blocks").
fn kind_of(ty: u64) -> &'static str {
    match ty {
        0x00..=0x03 => "spacer",
        0x04 => "size hint",
        0x10..=0x17 => "metadata",
        0x20..=0x37 => "leaf data",
        0x40..=0x4b => "non-leaf data",
        0x50 => "dictionary (registered)",
        0x51..=0x57 => "dictionary",
        0x58..=0x5f => "import",
        0x60 => "module (registered)",
        0x61..=0x63 => "module",
        0x64..=0x67 => "resource",
        0x70..=0x77 => "formula",
        0x80..=0x87 => "mirror",
        _ if !critical(ty) => "ignored",
        _ => "unknown",
    }
}

impl Inspection<'_> {
    /// Of segment `segment`, the formulas making `contents` in full, a line of each node
    /// ([`super::listing`]'s graph), indented from `indent`.
    pub fn formulas(&self, segment: usize, contents: &[Content], indent: usize) -> Decoded<Vec<String>> {
        let p = &self.parsed[segment];
        let names = &self.names[segment];
        let name_of = |m: usize| names[m].clone();
        let (g, leaves) = graph(p, &name_of)?;
        Ok(contents
            .iter()
            .map(|c| {
                let mut out = String::new();
                match c.leaf {
                    Some(l) if (c.output as usize) < leaves[l].len() => {
                        let mut seen = HashSet::new();
                        g.dump(leaves[l][c.output as usize], indent, &p.non_leaf_at, &mut seen, &mut out);
                    }
                    _ => out = format!("{:indent$}(no such leaf block or output)\n", ""),
                }
                out
            })
            .collect())
    }

    /// By segment and entry number, what each entry is in the archive as it is: segments
    /// merged as [`super::open`] merges them (spec, "Merging segments"), an [`Origin`] being a
    /// segment and an entry number (the root 0). An earlier segment's root is merged into the
    /// next one's.
    pub fn merged(&self) -> Vec<Vec<State>> {
        let given = |e: &Entry| e.members.is_some();
        let roots: Vec<bool> = self.segments.iter().map(|g| g.entries.first().is_some_and(given)).collect();
        let shapes: Vec<Vec<Shape>> = (self.segments.iter())
            .map(|g| {
                (g.entries.iter().skip(1))
                    .map(|e| Shape {
                        parent: e.parent.checked_sub(1),
                        name: e.names.last().expect("not the root"),
                        kind: match e.node {
                            Node::Dir => Kind::Dir,
                            Node::Tombstone => Kind::Tombstone,
                            _ => Kind::Other,
                        },
                        given: given(e),
                    })
                    .collect()
            })
            .collect();
        // (by entry number, past the root)
        let number = |o: Origin| Origin { segment: o.segment, entry: o.entry + 1 };
        let mut states: Vec<Vec<State>> = (self.segments.iter())
            .map(|g| {
                (g.entries.iter())
                    .map(|e| match e.node {
                        Node::Tombstone => State::Tombstone,
                        _ => State::Current { extra: None },
                    })
                    .collect()
            })
            .collect();
        let r = tree::resolve(&roots, &shapes, &mut |o, f| {
            let f = match f {
                Fate::Replaced(b) => Fate::Replaced(number(b)),
                Fate::Removed(b) => Fate::Removed(number(b)),
                Fate::Merged(b) => Fate::Merged(number(b)),
                Fate::WithAncestor(b) => Fate::WithAncestor(number(b)),
            };
            states[o.segment][o.entry + 1] = State::Gone(f);
        });
        let has = |o: Origin| given(&self.segments[o.segment].entries[o.entry]);
        for e in r.entries {
            let (node, extra) = (number(e.node), number(e.extra));
            states[node.segment][node.entry] = State::Current { extra: has(extra).then_some(extra) };
        }
        // the roots: each merged into the next
        let n = states.iter().filter(|s| !s.is_empty()).count();
        for (k, s) in states.iter_mut().enumerate().filter(|(_, s)| !s.is_empty()) {
            s[0] = match k + 1 < n {
                true => State::Gone(Fate::Merged(Origin { segment: k + 1, entry: 0 })),
                false => State::Current { extra: r.root.map(|segment| Origin { segment, entry: 0 }) },
            };
        }
        states
    }

    /// Of an archive without a metadata block, its contents: each output of each leaf block in
    /// order, the file being all of them one after another.
    pub fn unnamed(&self) -> Vec<Content> {
        let seg = &self.segments[0];
        (seg.leaves.iter().enumerate())
            .flat_map(|(l, leaf)| {
                (0..leaf.outputs).map(move |o| Content {
                    leaf: Some(l),
                    offset: 0,
                    output: o,
                    size: 0,
                    dictionary: leaf.dictionary,
                })
            })
            .collect()
    }
}

/// Standalone archives `archives` (unencrypted, each of a metadata block) joined as the segments
/// of one, in order: their blocks one after another, each metadata block (stored again, not
/// compressed) pointing to the one before. For tests of segments updating the paths of those
/// before them: no encoder writes such (`aqr -S` puts each file in one segment alone).
/// Panics where a previous metadata offset would take a field width other than the one its
/// block was written with (archives of about the same sizes join).
#[doc(hidden)]
pub fn join_segments(archives: &[&[u8]]) -> Vec<u8> {
    use super::{block_of, checksum_of, decoded, footer_with};
    let width = |n: u64| match n {
        0 => 0,
        1..0x100 => 1,
        0x100..0x1_0000 => 2,
        0x1_0000..0x1_0000_0000 => 4,
        _ => 8,
    };
    let mut out = HEADER_MAGIC.to_vec();
    let mut prev_meta = HEADER_MAGIC.len() - 1;
    for c in archives {
        let fields = u16::from_le_bytes([c[c.len() - 2], c[c.len() - 1]]) as usize;
        let d = &c[HEADER_MAGIC.len()..c.len() - 2 - fields - 8];
        let framed = frame(d).expect("an archive");
        let &(start, ty, data, _) = framed.iter().rev().find(|b| (0x10..=0x17).contains(&b.1)).expect("metadata");
        out.extend_from_slice(&d[..start]);
        let meta = decoded(data, ty & 0x04 != 0, true).expect("a metadata block");
        let mut at = 0;
        let old = leb(&meta, &mut at).unwrap();
        let prev = (out.len() - prev_meta) as u64;
        assert_eq!(width(old), width(prev), "a previous metadata offset of another width");
        let mut m = Vec::new();
        aqr_common::varint::put(&mut m, prev);
        m.extend_from_slice(&meta[at..]);
        prev_meta = out.len();
        block_of(&mut out, 0x10, &[&m, &checksum_of(&[&m]).to_le_bytes()]);
    }
    let end = out.len();
    footer_with(&mut out, end - HEADER_MAGIC.len(), (end + FOOTER_MAGIC.len() + 2 - prev_meta) as u64);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Formula;
    use crate::aqr::{ContentAt, Encoder, EntryNode, Leaf, Level, SegmentWriter, Time, Tree};

    /// An archive of a segment of each of `segments`, of files (name, bytes, mtime) each.
    fn archive(segments: &[&[(&str, &[u8], i64)]]) -> Vec<u8> {
        let enc = Encoder::new(Level::STORED);
        let mut w = SegmentWriter::new();
        let mut out = SegmentWriter::header().to_vec();
        for (k, files) in segments.iter().enumerate() {
            let mut f = Formula::default();
            let leaves: Vec<Leaf> = (files.iter())
                .map(|(_, b, _)| Leaf { outputs: vec![f.push("t", b.to_vec())], sums: vec![None] })
                .collect();
            let b = w.batch(&enc, &f, &leaves).unwrap();
            w.commit(&b);
            out.extend_from_slice(&b.bytes);
            let mut t = Tree::default();
            let mut contents = Vec::new();
            for (i, (name, bytes, mtime)) in files.iter().enumerate() {
                let extra = Extra { mtime: Some(Time::new(*mtime, 0)), ..Extra::default() };
                t.entries.push(crate::aqr::Entry {
                    parent: None,
                    name: name.as_bytes().to_vec(),
                    node: EntryNode::File(i),
                    extra,
                });
                contents.push(ContentAt { leaf: b.leaf_at[i], output: 0, size: bytes.len() as u64 });
            }
            if k + 1 < segments.len() {
                out.extend_from_slice(&w.close(&enc, &t, &contents).unwrap());
            } else {
                out.extend_from_slice(&w.finish(&enc, &t, &contents).unwrap().0);
            }
        }
        out
    }

    #[test]
    fn segments_inspected() {
        let a: &[(&str, &[u8], i64)] = &[("a", &[1; 300], 10), ("b", &[2; 300], 20)];
        let joined = archive(&[a, &[("b", &[3; 290], 30)]]);
        // (open merges them: the later "b" replaces the earlier)
        let opened = crate::aqr::open(&joined, &crate::aqr::NoModules).unwrap();
        assert_eq!(opened.tree().unwrap().entries.len(), 2);
        let i = inspect(&joined, false, &|_| None, None).unwrap();
        assert_eq!(i.segments.len(), 2);
        for (k, s) in i.segments.iter().enumerate() {
            assert!(s.problems.is_empty(), "{k}: {:?}", s.problems);
        }
        let names = |s: &Segment| s.entries.iter().map(|e| e.names.concat()).collect::<Vec<_>>();
        assert_eq!(names(&i.segments[0]), [b"".to_vec(), b"a".to_vec(), b"b".to_vec()]);
        assert_eq!(names(&i.segments[1]), [b"".to_vec(), b"b".to_vec()]);
        let e = &i.segments[1].entries[1];
        assert_eq!(e.extra.mtime, Some(Time::new(30, 0)));
        let Node::File(c) = e.node else { panic!("a file") };
        assert_eq!(c.size, 290);
        let f = i.formulas(1, &[c], 2).unwrap();
        assert!(f[0].contains("290 B"), "{}", f[0]);
        let first = i.segments[0].metadata.as_ref().unwrap().at;
        assert_eq!(i.segments[1].metadata.as_ref().unwrap().prev_at, Some(first));
        // the blocks of each segment
        assert!(i.blocks.iter().all(|b| b.segment.is_some()));
        // the first alone, as it was
        let alone = archive(&[a]);
        let one = inspect(&alone, false, &|_| None, None).unwrap();
        assert_eq!(one.segments.len(), 1);
        assert!(one.segments[0].problems.is_empty());
    }

    /// A node of a segment's tree for [`segmented`].
    enum N {
        Dir(Option<u32>),
        File(&'static [u8]),
        Link,
    }

    /// An archive of a segment of each of `segments`, of entries (parent, name, node) each.
    fn segmented(segments: &[Vec<(Option<usize>, &str, N)>]) -> Vec<u8> {
        let enc = Encoder::new(Level::STORED);
        let mut w = SegmentWriter::new();
        let mut out = SegmentWriter::header().to_vec();
        for (k, entries) in segments.iter().enumerate() {
            let mut f = Formula::default();
            let files: Vec<&[u8]> =
                entries.iter().filter_map(|e| if let N::File(b) = e.2 { Some(b) } else { None }).collect();
            let leaves: Vec<Leaf> =
                files.iter().map(|b| Leaf { outputs: vec![f.push("t", b.to_vec())], sums: vec![None] }).collect();
            let b = w.batch(&enc, &f, &leaves).unwrap();
            w.commit(&b);
            out.extend_from_slice(&b.bytes);
            let mut t = Tree::default();
            for (parent, name, n) in entries {
                let (node, mode) = match n {
                    N::Dir(mode) => (EntryNode::Dir, *mode),
                    N::File(_) => (
                        EntryNode::File(t.entries.iter().filter(|e| matches!(e.node, EntryNode::File(_))).count()),
                        None,
                    ),
                    N::Link => (EntryNode::Link(b"t".to_vec()), None),
                };
                let extra = Extra { mode, ..Extra::default() };
                t.entries.push(crate::aqr::Entry { parent: *parent, name: name.as_bytes().to_vec(), node, extra });
            }
            let contents: Vec<ContentAt> = (files.iter().enumerate())
                .map(|(j, bytes)| ContentAt { leaf: b.leaf_at[j], output: 0, size: bytes.len() as u64 })
                .collect();
            if k + 1 < segments.len() {
                out.extend_from_slice(&w.close(&enc, &t, &contents).unwrap());
            } else {
                out.extend_from_slice(&w.finish(&enc, &t, &contents).unwrap().0);
            }
        }
        out
    }

    /// The entries of the segments merged as open merges them: a directory merged (its extra
    /// kept where a later one has none, replaced where it has one), a file replaced, a
    /// directory replaced by a link (all under it with it) and the link by a directory again.
    #[test]
    fn merged_as_opened() {
        let c = segmented(&[
            vec![
                (None, "d", N::Dir(Some(0o700))),
                (None, "l", N::Dir(None)),
                (Some(0), "a", N::File(b"a")),
                (Some(0), "s", N::Dir(None)),
                (Some(3), "b", N::File(b"b")),
                (Some(1), "c", N::File(b"c")),
                (None, "x", N::File(b"x")),
            ],
            vec![(None, "d", N::Dir(None)), (None, "l", N::Link), (None, "x", N::File(b"x2"))],
            vec![(None, "d", N::Dir(Some(0o755))), (None, "l", N::Dir(None)), (Some(1), "g", N::File(b"g"))],
        ]);
        let i = inspect(&c, false, &|_| None, None).unwrap();
        let states = i.merged();
        // the tree open makes, and the current entries: path, kind and mode
        let opened = crate::aqr::open(&c, &crate::aqr::NoModules).unwrap();
        let t = opened.tree().unwrap();
        let path = |mut k: usize| {
            let mut p = t.entries[k].name.clone();
            while let Some(u) = t.entries[k].parent {
                p = [&t.entries[u].name[..], b"/", &p].concat();
                k = u;
            }
            p
        };
        let mut want: Vec<(Vec<u8>, bool, Option<u32>)> = (0..t.entries.len())
            .map(|k| (path(k), t.entries[k].node == EntryNode::Dir, t.entries[k].extra.mode))
            .collect();
        let mut got = Vec::new();
        for (k, g) in i.segments.iter().enumerate() {
            for (e, entry) in g.entries.iter().enumerate().skip(1) {
                if let State::Current { extra } = states[k][e] {
                    let mode = extra.and_then(|x| i.segments[x.segment].entries[x.entry].extra.mode);
                    got.push((entry.names.join(&b"/"[..]), entry.node == Node::Dir, mode));
                }
            }
        }
        want.sort();
        got.sort();
        assert_eq!(got, want);
        let o = |segment, entry| Origin { segment, entry };
        // (by entry number: the directories first, as they are encoded)
        let number = |k: usize, names: &[&str]| {
            let names: Vec<Vec<u8>> = names.iter().map(|n| n.as_bytes().to_vec()).collect();
            i.segments[k].entries.iter().position(|e| e.names == names).unwrap()
        };
        let (d0, d1, d2) = (number(0, &["d"]), number(1, &["d"]), number(2, &["d"]));
        assert_eq!(states[0][d0], State::Gone(Fate::Merged(o(1, d1))));
        assert_eq!(states[1][d1], State::Gone(Fate::Merged(o(2, d2))));
        assert_eq!(states[2][d2], State::Current { extra: Some(o(2, d2)) });
        let (l0, l1) = (number(0, &["l"]), number(1, &["l"]));
        assert_eq!(states[0][l0], State::Gone(Fate::Replaced(o(1, l1))));
        assert_eq!(states[0][number(0, &["l", "c"])], State::Gone(Fate::WithAncestor(o(0, l0))));
        assert_eq!(states[1][l1], State::Gone(Fate::Replaced(o(2, number(2, &["l"])))));
        assert_eq!(states[0][number(0, &["x"])], State::Gone(Fate::Replaced(o(1, number(1, &["x"])))));
        // (d/s and d/a, under a directory merged, left)
        assert!(matches!(states[0][number(0, &["d", "s", "b"])], State::Current { .. }));
        assert_eq!(states[0][0], State::Gone(Fate::Merged(o(1, 0))));
        assert!(matches!(states[2][0], State::Current { .. }));
    }

    /// A formula in full: each node a line, indented by depth, an application of several
    /// outputs numbered and its inputs told once; the modules by their names.
    #[test]
    fn formula_dumped() {
        use crate::aqr::tests::{CUT, REV};
        use crate::aqr::{encode, stand_in_name};
        let mut f = Formula::default();
        let a = f.push("a", b"abcdefgh".to_vec());
        let cut = crate::Apply {
            module: CUT,
            resources: vec![],
            params: vec![],
            inputs: vec![a],
            len: 0,
            memory_limit: crate::NO_LIMIT,
            task_limits: Vec::new(),
        };
        let [o0, o1] = <[crate::Node; 2]>::try_from(f.share(cut, vec![3, 5])).unwrap();
        let rev = crate::Node::apply(REV, vec![1, 2, 3], vec![o0], 1);
        f.root = crate::Node::concat([o1, rev]);
        let c = encode(&f, None, Level::Zstd(3)).unwrap();
        let i = inspect(&c, false, &|w| stand_in_name(w).map(String::from), None).unwrap();
        assert!(i.segments[0].metadata.is_none());
        let got = i.formulas(0, &i.unnamed(), 2).unwrap().concat();
        let lines: Vec<&str> = got.lines().collect();
        assert!(lines[0].starts_with("  cat  6 B"), "{got}");
        assert!(lines[1].starts_with("    cut#2  5 B  @") && lines[1].contains("module #"), "{got}");
        assert!(lines[2].starts_with("      slice  8 B  of non-leaf block #1 at "), "{got}");
        assert!(lines[3].starts_with("    rev  1 B") && lines[3].contains("params 3 B 010203"), "{got}");
        assert!(lines[4].starts_with("      cut#1  3 B  @") && lines[4].ends_with("(inputs as above)"), "{got}");
        assert_eq!(lines.len(), 5, "{got}");
    }
}
