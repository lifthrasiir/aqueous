//! An archive of several files, as one segment, written a batch of leaves at a time
//! ([`SegmentWriter`]; all of them at once, [`Encoder::encode_segment`]): each leaf a formula
//! block of the outputs of a [`Leaf`] (or a leaf data block, where it is all of a stream of its
//! own), after the formula blocks that slice them the non-leaf blocks of the streams, each module
//! and resource block before the first leaf taking it; the metadata block of the [`Tree`] last.
//!
//! Leaves that slice a stream together are laid out together, as a group: their formula
//! blocks, then the streams any of them slices (spec, "Block order"). A stream sliced by
//! several leaves holds them solid; the encoder is given the streams as they are, and decides
//! nothing of what goes together. A batch is checked alone ([`SegmentWriter::standalone`])
//! before it is written: its blocks with the module and resource blocks before them make an
//! archive of a single file, its leaves' outputs one after another.

use std::collections::HashSet;

use aqr_common::varint;

use super::tree::{self, EntryNode, FileParam, Tree};
use super::{
    CHECKSUM, Emit, Encoder, FOOTER_MAGIC, HEADER_MAGIC, Kind, MODULE, SWAP, always_compressed, block, footer,
    footer_with, formula_block, wasm,
};
use crate::{Error, Formula, Module, Node, Resource, Result};

/// The content of one or more files: outputs over the streams (and the applications of several
/// outputs) of a formula.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Leaf {
    pub outputs: Vec<Node>,
    /// The checksum of each output ([`super::checksum`]), checked by the formula block.
    pub sums: Vec<Option<u64>>,
}

/// What a segment holds: `f`'s streams and applications of several outputs (its root is not
/// used), the leaves over them, the entries, and where each content of the entries is (its
/// leaf and output).
pub struct Segment<'a> {
    pub f: &'a Formula,
    pub leaves: &'a [Leaf],
    pub tree: &'a Tree,
    pub contents: &'a [(usize, usize)],
}

/// A block of the archive of a segment, and its bytes (with its type and size).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placed {
    Module,
    Resource,
    /// The formula block of a leaf (of its batch).
    Formula(usize),
    /// The leaf data block of a leaf, all of a stream.
    Data(usize),
    /// The non-leaf block of a stream (of the formula of its batch).
    NonLeaf(usize),
    Metadata,
}

/// The blocks of an archive in order, each what it is and its bytes.
pub type Layout = Vec<(Placed, usize)>;

/// A segment being written: where it is, and the modules and resources of its blocks so far.
pub struct SegmentWriter {
    /// Bytes of the archive so far, its header magic among them.
    at: usize,
    modules: Vec<Module>,
    module_at: Vec<usize>,
    resources: Vec<Resource>,
    resource_at: Vec<usize>,
    /// The module and resource blocks so far, one after another.
    prefix: Vec<u8>,
}

/// The blocks of a batch of leaves, to write after those before ([`SegmentWriter::commit`]).
pub struct Batch {
    pub bytes: Vec<u8>,
    /// Where each leaf block is in the archive.
    pub leaf_at: Vec<usize>,
    pub layout: Layout,
    /// The modules and resources it adds, and where their blocks are in `bytes`.
    modules: Vec<Module>,
    resources: Vec<Resource>,
    added: Vec<std::ops::Range<usize>>,
}

/// Where a content of an archive is: the leaf block (where it starts in the archive), its output
/// and its size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContentAt {
    pub leaf: usize,
    pub output: usize,
    pub size: u64,
}

impl Default for SegmentWriter {
    fn default() -> SegmentWriter {
        SegmentWriter::new()
    }
}

impl SegmentWriter {
    /// A segment of no blocks yet, after the header ([`SegmentWriter::header`]).
    pub fn new() -> SegmentWriter {
        SegmentWriter {
            at: HEADER_MAGIC.len(),
            modules: Vec::new(),
            module_at: Vec::new(),
            resources: Vec::new(),
            resource_at: Vec::new(),
            prefix: Vec::new(),
        }
    }

    /// The bytes of the archive before its blocks.
    pub fn header() -> &'static [u8] {
        &HEADER_MAGIC
    }

    /// Bytes of the archive so far.
    pub fn len(&self) -> usize {
        self.at
    }

    pub fn is_empty(&self) -> bool {
        self.at == HEADER_MAGIC.len()
    }

    /// The blocks of `leaves` over `f`'s streams and applications of several outputs (its root
    /// not used), to follow those so far. Fails on slices out of their streams and on outputs
    /// not taken once each (by one leaf).
    pub fn batch(&self, enc: &Encoder, f: &Formula, leaves: &[Leaf]) -> Result<Batch> {
        let mut check = f.clone();
        check.root = Node::Concat(leaves.iter().flat_map(|l| l.outputs.iter().cloned()).collect());
        check.check_outputs()?;
        for (i, l) in leaves.iter().enumerate() {
            if l.outputs.is_empty() || l.sums.len() != l.outputs.len() {
                return Err(Error::new(format!("leaf #{i} of no outputs, or not as many checksums")));
            }
        }
        let mut bad = None;
        check.visit(&mut |n| {
            if let &Node::Slice { stream, start, len } = n
                && f.streams.get(stream).is_none_or(|s| start.checked_add(len).is_none_or(|e| e > s.data.len() as u64))
            {
                bad = Some(Error::new(format!("slice [{start}, +{len}) out of stream #{stream}")));
            }
        });
        if let Some(e) = bad {
            return Err(e);
        }

        let uses: Vec<Uses> = leaves.iter().map(|l| uses(f, l)).collect();
        // leaves of a stream of their own, all of it (or of nothing)
        let mut slicers = vec![0usize; f.streams.len()];
        uses.iter().for_each(|u| u.streams.iter().for_each(|&st| slicers[st] += 1));
        let data_leaf = |i: usize| -> Option<Option<usize>> {
            let l = &leaves[i];
            match (&l.outputs[..], uses[i].shared.is_empty()) {
                ([n], true) if n.is_empty() => Some(None),
                ([Node::Slice { stream, start: 0, len }], true)
                    if slicers[*stream] == 1 && *len == f.streams[*stream].data.len() as u64 =>
                {
                    Some(Some(*stream))
                }
                _ => None,
            }
        };

        // the groups of leaves slicing a stream together, by their first leaf
        let mut group: Vec<usize> = (0..leaves.len()).collect();
        fn find(g: &mut [usize], i: usize) -> usize {
            let mut r = i;
            while g[r] != r {
                r = g[r];
            }
            g[i] = r;
            r
        }
        let mut owner = vec![usize::MAX; f.streams.len()];
        for (i, u) in uses.iter().enumerate() {
            for &st in &u.streams {
                match owner[st] {
                    usize::MAX => owner[st] = i,
                    o => {
                        let (a, b) = (find(&mut group, o), find(&mut group, i));
                        group[a.max(b)] = a.min(b);
                    }
                }
            }
        }
        let roots: Vec<usize> = (0..leaves.len()).map(|i| find(&mut group, i)).collect();

        // the streams compressed at once, then laid out
        let mut written = vec![false; f.streams.len()];
        uses.iter().for_each(|u| u.streams.iter().for_each(|&st| written[st] = true));
        let todo: Vec<&[u8]> = (0..f.streams.len())
            .filter(|&st| written[st] && f.streams[st].kind != Kind::Compressed)
            .map(|st| &f.streams[st].data[..])
            .collect();
        enc.precompress(&todo)?;

        let level = enc.base_level();
        let mut b = Batch {
            bytes: Vec::new(),
            leaf_at: vec![0; leaves.len()],
            layout: Vec::new(),
            modules: Vec::new(),
            resources: Vec::new(),
            added: Vec::new(),
        };
        let (mut modules, mut resources): (Vec<Module>, Vec<Resource>) = (self.modules.clone(), self.resources.clone());
        // (the non-leaf blocks counted from the batch's first: only their deltas are stored)
        let mut ordinals = vec![None; f.streams.len()];
        let mut non_leaf = 0u64;
        let mut done = vec![false; leaves.len()];
        let out = &mut b.bytes;
        for first in 0..leaves.len() {
            if done[first] {
                continue;
            }
            let members: Vec<usize> = (first..leaves.len()).filter(|&i| roots[i] == roots[first]).collect();
            // the modules and resources of the group not written yet
            for &i in &members {
                for &m in &uses[i].modules {
                    if !modules.contains(&m) {
                        let at = out.len();
                        block(out, MODULE, &always_compressed(&mut enc.cctx(), &[&wasm(m)], level, enc.threads)?);
                        b.layout.push((Placed::Module, out.len() - at));
                        b.added.push(at..out.len());
                        modules.push(m);
                        b.modules.push(m);
                    }
                }
                for r in &uses[i].resources {
                    if !resources.contains(r) {
                        let at = out.len();
                        enc.resource_block(out, r)?;
                        b.layout.push((Placed::Resource, out.len() - at));
                        b.added.push(at..out.len());
                        resources.push(r.clone());
                        b.resources.push(r.clone());
                    }
                }
            }
            // the streams of the group, in their order, as the blocks after its formula blocks
            let mut streams: Vec<usize> = members
                .iter()
                .filter(|&&i| data_leaf(i).is_none())
                .flat_map(|&i| uses[i].streams.iter().copied())
                .collect::<HashSet<_>>()
                .into_iter()
                .collect();
            streams.sort_unstable();
            streams.iter().enumerate().for_each(|(k, &st)| ordinals[st] = Some(non_leaf + k as u64));
            let refs: Vec<&Resource> = resources.iter().collect();
            for &i in &members {
                done[i] = true;
                let at = out.len();
                b.leaf_at[i] = self.at + at;
                match data_leaf(i) {
                    Some(stream) => {
                        let data: &[u8] = stream.map_or(&[], |st| &f.streams[st].data);
                        let stored = stream.is_some_and(|st| f.streams[st].kind == Kind::Compressed);
                        enc.content_block(out, true, &[data], stored)?;
                        b.layout.push((Placed::Data(i), out.len() - at));
                    }
                    None => {
                        let data = enc.leaf_formula(f, &leaves[i], &uses[i], non_leaf, &ordinals, &modules, &refs);
                        let n = leaves[i].outputs.len() as u64;
                        formula_block(out, n, &always_compressed(&mut enc.cctx(), &[&data], level, enc.threads)?);
                        b.layout.push((Placed::Formula(i), out.len() - at));
                    }
                }
            }
            for &st in &streams {
                let at = out.len();
                let stored = f.streams[st].kind == Kind::Compressed;
                enc.content_block(out, false, &[&f.streams[st].data], stored)?;
                b.layout.push((Placed::NonLeaf(st), out.len() - at));
            }
            non_leaf += streams.len() as u64;
        }
        Ok(b)
    }

    /// An archive of a single file of batch `b` (the blocks so far not in it but the module and
    /// resource blocks): its leaves' outputs one after another.
    pub fn standalone(&self, b: &Batch) -> Vec<u8> {
        let mut out = Vec::with_capacity(HEADER_MAGIC.len() + self.prefix.len() + b.bytes.len() + 32);
        out.extend_from_slice(&HEADER_MAGIC);
        out.extend_from_slice(&self.prefix);
        out.extend_from_slice(&b.bytes);
        let blocks = out.len() - HEADER_MAGIC.len();
        footer(&mut out, blocks);
        out
    }

    /// Takes batch `b` as written after the blocks so far.
    pub fn commit(&mut self, b: &Batch) {
        for r in &b.added {
            self.prefix.extend_from_slice(&b.bytes[r.clone()]);
        }
        // (the added blocks in the order of their kinds, modules and resources each in order)
        let mut adds = b.added.iter();
        let (mut mi, mut ri) = (0, 0);
        for &(p, _) in &b.layout {
            match p {
                Placed::Module => {
                    self.module_at.push(self.at + adds.next().unwrap().start);
                    self.modules.push(b.modules[mi]);
                    mi += 1;
                }
                Placed::Resource => {
                    self.resource_at.push(self.at + adds.next().unwrap().start);
                    self.resources.push(b.resources[ri].clone());
                    ri += 1;
                }
                _ => {}
            }
        }
        self.at += b.bytes.len();
    }

    /// The end of the archive: the metadata block of `t`, its contents where `contents` tells
    /// (by [`EntryNode::File`]), and the footer. Fails on a tree the spec does not allow.
    pub fn finish(&self, enc: &Encoder, t: &Tree, contents: &[ContentAt]) -> Result<(Vec<u8>, usize)> {
        tree::check(t, contents.len()).map_err(Error::new)?;
        let mut files = vec![None; t.entries.len()];
        for (i, e) in t.entries.iter().enumerate() {
            if let EntryNode::File(c) = e.node {
                let c = contents[c];
                files[i] = Some(FileParam { leaf: c.leaf, output: c.output as u64, size: c.size });
            }
        }
        let meta = tree::metadata(t, self.at, &self.module_at, &self.resource_at, &files);
        let mut out = Vec::new();
        block(&mut out, METADATA_ZSTD, &always_compressed(&mut enc.cctx(), &[&meta], enc.base_level(), enc.threads)?);
        let metadata = out.len();
        let end = self.at + out.len();
        // (from the start of the footer's fields: after the magic and the size)
        let back = end + FOOTER_MAGIC.len() + 2 - self.at;
        footer_with(&mut out, end - HEADER_MAGIC.len(), back as u64);
        Ok((out, metadata))
    }
}

impl Encoder {
    /// The archive of segment `s`, with what each of its blocks is: all of its leaves one batch.
    /// Fails on slices out of their streams, on outputs not taken once each (by one leaf), and
    /// on a tree the spec does not allow.
    pub fn encode_segment(&self, s: &Segment) -> Result<(Vec<u8>, Layout)> {
        tree::check(s.tree, s.contents.len()).map_err(Error::new)?;
        for &(leaf, output) in s.contents {
            if s.leaves.get(leaf).is_none_or(|l| output >= l.outputs.len()) {
                return Err(Error::new(format!("no output {output} of leaf #{leaf}")));
            }
        }
        let mut w = SegmentWriter::new();
        let b = w.batch(self, s.f, s.leaves)?;
        w.commit(&b);
        let contents: Vec<ContentAt> = s
            .contents
            .iter()
            .map(|&(l, o)| ContentAt { leaf: b.leaf_at[l], output: o, size: s.leaves[l].outputs[o].len() })
            .collect();
        let (end, metadata) = w.finish(self, s.tree, &contents)?;
        let mut layout = b.layout;
        layout.push((Placed::Metadata, metadata));
        Ok(([&HEADER_MAGIC[..], &b.bytes, &end].concat(), layout))
    }

    /// The formula data of `leaf`, its streams' non-leaf blocks at `ordinals` (from `first`,
    /// the first after its formula block), among the segment's `modules` and `resources`.
    #[allow(clippy::too_many_arguments)]
    fn leaf_formula(
        &self,
        f: &Formula,
        leaf: &Leaf,
        u: &Uses,
        first: u64,
        ordinals: &[Option<u64>],
        modules: &[Module],
        resources: &[&Resource],
    ) -> Vec<u8> {
        let mut data = Vec::new();
        // the blocks it slices, by their ordinals' deltas, and its own index of each stream
        let mut sliced: Vec<(u64, usize)> = u.streams.iter().map(|&st| (ordinals[st].unwrap(), st)).collect();
        sliced.sort_unstable();
        let mut local = vec![None; f.streams.len()];
        varint::put(&mut data, sliced.len() as u64);
        let mut next = first;
        for (k, &(o, st)) in sliced.iter().enumerate() {
            varint::put(&mut data, o - next);
            next = o + 1;
            local[st] = Some(k as u64);
        }
        let own_resources: Vec<&Resource> = u.resources.iter().collect();
        varint::put(&mut data, own_resources.len() as u64);
        for r in &own_resources {
            varint::put(&mut data, resources.iter().position(|s| s == r).unwrap() as u64);
        }
        varint::put(&mut data, u.modules.len() as u64);
        for m in &u.modules {
            varint::put(&mut data, modules.iter().position(|n| n == m).unwrap() as u64);
        }
        let mut e =
            Emit { ordinals: &local, modules: &u.modules, resources: &own_resources, out: &mut data, stack: vec![] };
        for &i in &u.shared {
            let s = &f.shared[i];
            s.apply.inputs.iter().for_each(|n| e.node(n));
            e.apply(&s.apply, &s.sizes);
            let k = e.stack.len() - s.sizes.len();
            (0..s.sizes.len()).for_each(|j| e.stack[k + j] = Some((i, j)));
        }
        leaf.outputs.iter().for_each(|n| e.node(n));
        debug_assert!(e.stack.iter().all(Option::is_none) && e.stack.len() == leaf.outputs.len());
        // each output checked where it is brought to the top, and put back
        let k = leaf.outputs.len();
        for (j, sum) in leaf.sums.iter().enumerate() {
            let Some(sum) = sum else { continue };
            let d = (k - 1 - j) as u64;
            if d > 0 {
                data.push(SWAP);
                varint::put(&mut data, d);
            }
            data.push(CHECKSUM);
            data.extend_from_slice(&sum.to_le_bytes());
            if d > 0 {
                data.push(SWAP);
                varint::put(&mut data, d);
            }
        }
        data
    }
}

/// A metadata block, Zstandard compressed, with a u8 size field.
const METADATA_ZSTD: u64 = 0x14;

/// What a leaf takes: the streams it slices and the applications of several outputs whose
/// outputs it takes (directly or through those of later ones), in order; the modules and
/// resources of its applications, in the order of their first.
struct Uses {
    streams: Vec<usize>,
    shared: Vec<usize>,
    modules: Vec<Module>,
    resources: Vec<Resource>,
}

fn uses(f: &Formula, leaf: &Leaf) -> Uses {
    let mut shared = vec![false; f.shared.len()];
    let mark = |n: &Node, shared: &mut Vec<bool>| {
        n.visit(&mut |n| {
            if let &Node::Output { shared: s, .. } = n {
                shared[s] = true;
            }
        })
    };
    leaf.outputs.iter().for_each(|n| mark(n, &mut shared));
    for i in (0..f.shared.len()).rev() {
        if shared[i] {
            f.shared[i].apply.inputs.iter().for_each(|n| mark(n, &mut shared));
        }
    }
    let shared: Vec<usize> = (0..f.shared.len()).filter(|&i| shared[i]).collect();
    let mut u = Uses { streams: Vec::new(), shared: Vec::new(), modules: Vec::new(), resources: Vec::new() };
    fn note(u: &mut Uses, a: &crate::Apply) {
        if !u.modules.contains(&a.module) {
            u.modules.push(a.module);
        }
        for r in &a.resources {
            if !u.resources.contains(r) {
                u.resources.push(r.clone());
            }
        }
    }
    fn visit(u: &mut Uses, n: &Node) {
        n.visit(&mut |n| match n {
            // (an empty slice is of no block)
            &Node::Slice { stream, len: 1.., .. } if !u.streams.contains(&stream) => u.streams.push(stream),
            Node::Apply(a) => note(u, a),
            _ => {}
        })
    }
    // (the applications of several outputs first, as they are emitted)
    for &i in &shared {
        let a = &f.shared[i].apply;
        a.inputs.iter().for_each(|n| visit(&mut u, n));
        note(&mut u, a);
    }
    leaf.outputs.iter().for_each(|n| visit(&mut u, n));
    u.shared = shared;
    u
}

#[cfg(test)]
mod tests {
    use super::super::tests::{CUT, REV, natives};
    use super::super::{Level, Value, checksum, decode, decode_value, invalid};
    use super::*;
    use crate::{Apply, Kind};
    use tree::{Entry, Extra, Time};

    fn entry(parent: Option<usize>, name: &str, node: EntryNode) -> Entry {
        Entry { parent, name: name.as_bytes().to_vec(), node, extra: Extra::default() }
    }

    /// A segment of two batches, each checked alone first: the second takes the modules the
    /// first wrote, and a content of the first.
    #[test]
    fn batches() {
        let mut f = Formula::default();
        let a = f.push("a", b"hello".to_vec());
        let b = f.push("b", b"xyz".to_vec());
        let enc = Encoder::new(Level::Zstd(3));
        let rev = |inputs| Node::apply(REV, vec![1, 2, 3], inputs, 8);
        let mut w = SegmentWriter::new();
        let mut out = SegmentWriter::header().to_vec();
        let mut at = Vec::new();
        for leaves in [
            vec![Leaf { outputs: vec![rev(vec![a.clone(), b.clone()])], sums: vec![None] }],
            vec![Leaf { outputs: vec![rev(vec![b, a])], sums: vec![Some(checksum(b"ollehzyx"))] }],
        ] {
            let batch = w.batch(&enc, &f, &leaves).unwrap();
            let want = if at.is_empty() { &b"zyxolleh"[..] } else { b"ollehzyx" };
            assert_eq!(decode(&w.standalone(&batch), &natives()).unwrap(), want);
            assert_eq!(batch.layout.iter().filter(|p| p.0 == Placed::Module).count(), at.is_empty() as usize);
            w.commit(&batch);
            out.extend_from_slice(&batch.bytes);
            at.push(batch.leaf_at[0]);
        }
        assert_eq!(w.len(), out.len());
        let mut t = Tree::default();
        t.entries.push(entry(None, "x", EntryNode::File(0)));
        t.entries.push(entry(None, "y", EntryNode::File(1)));
        let contents: Vec<ContentAt> = at.iter().map(|&leaf| ContentAt { leaf, output: 0, size: 8 }).collect();
        out.extend_from_slice(&w.finish(&enc, &t, &contents).unwrap().0);
        let Ok(Value::Tree(_, files)) = decode_value(&out, &natives()) else { panic!() };
        assert_eq!(files, [b"zyxolleh".to_vec(), b"ollehzyx".to_vec()]);
    }

    /// Files of leaves of every kind: of a stream of their own (a leaf data block), solid in a
    /// stream with others, empty, an application, and the outputs of an application of
    /// several outputs; directories, a link, a file of the same content as another, extras.
    #[test]
    fn segments() {
        let mut f = Formula::default();
        let own = f.push("own", b"all of its own".to_vec());
        f.push("solid", b"0123456789".to_vec());
        let solid = |start, len| Node::slice(1, start, len);
        let x = f.push_as("x", Kind::Compressed, b"hello".to_vec());
        let cut = Apply { module: CUT, resources: vec![], params: vec![], inputs: vec![solid(2, 6)], len: 0 };
        let outs = f.share(cut, vec![2, 4]);
        let rev = Node::apply(REV, vec![1, 2, 3], vec![x, solid(0, 2)], 7);
        let leaf = |outputs: Vec<Node>, sums: Vec<Option<u64>>| Leaf { outputs, sums };
        let want: [&[u8]; 6] = [b"all of its own", b"01", b"", b"10olleh", b"23", b"4567"];
        let leaves = [
            leaf(vec![own], vec![None]),
            leaf(vec![solid(0, 2)], vec![Some(checksum(want[1]))]),
            leaf(vec![Node::Concat(vec![])], vec![None]),
            leaf(vec![rev], vec![Some(checksum(want[3]))]),
            leaf(outs, vec![Some(checksum(want[4])), Some(checksum(want[5]))]),
        ];
        let contents = [(0, 0), (1, 0), (2, 0), (3, 0), (4, 0), (4, 1)];
        let mut tree = Tree::default();
        tree.root.mode = Some(0o700);
        tree.entries.push(entry(None, "d", EntryNode::Dir));
        tree.entries.push(entry(Some(0), "e", EntryNode::Dir));
        for (c, name) in ["own", "solid", "empty", "rev", "cut0", "cut1"].iter().enumerate() {
            tree.entries.push(entry(Some(c % 2), name, EntryNode::File(c)));
        }
        tree.entries.push(entry(None, "same", EntryNode::File(3)));
        tree.entries.push(entry(Some(1), "link", EntryNode::Link(b"../own".to_vec())));
        tree.entries[2].extra = Extra { mtime: Some(Time { secs: 1_700_000_000, nanos: 5 }), mode: Some(0o644) };
        let seg = Segment { f: &f, leaves: &leaves, tree: &tree, contents: &contents };
        let enc = Encoder::new(Level::Zstd(3));
        let (c, placed) = enc.encode_segment(&seg).unwrap();
        let footer = 10 + u16::from_le_bytes([c[c.len() - 2], c[c.len() - 1]]) as usize;
        assert_eq!(placed.iter().map(|p| p.1).sum::<usize>() + HEADER_MAGIC.len() + footer, c.len());
        let kinds: Vec<Placed> = placed.iter().map(|p| p.0).collect();
        assert_eq!(
            kinds,
            [
                Placed::Data(0),
                Placed::Module,
                Placed::Module,
                Placed::Formula(1),
                Placed::Formula(3),
                Placed::Formula(4),
                Placed::NonLeaf(1),
                Placed::NonLeaf(2),
                Placed::Data(2),
                Placed::Metadata
            ]
        );
        let Ok(Value::Tree(t, files)) = decode_value(&c, &natives()) else { panic!() };
        assert_eq!(t.root, tree.root);
        // in the order of their numbers: directories, files, the link
        let names: Vec<&[u8]> = t.entries.iter().map(|e| &e.name[..]).collect();
        let want_names: [&[u8]; 10] =
            [b"d", b"e", b"own", b"solid", b"empty", b"rev", b"cut0", b"cut1", b"same", b"link"];
        assert_eq!(names, want_names);
        assert_eq!(t.entries[2].extra, tree.entries[2].extra);
        assert_eq!(t.entries[9].node, EntryNode::Link(b"../own".to_vec()));
        assert_eq!(t.entries[9].parent, Some(1));
        let got: Vec<&[u8]> = t
            .entries
            .iter()
            .filter_map(|e| if let EntryNode::File(c) = e.node { Some(&files[c][..]) } else { None })
            .collect();
        assert_eq!(got, [want[0], want[1], want[2], want[3], want[4], want[5], want[3]]);
        assert_eq!(files.len(), 6);
        assert_eq!(decode(&c, &natives()), super::super::resource("an archive of several files"));

        // a checksum not that of the output
        let mut bad = leaves.clone();
        bad[4].sums[0] = Some(1);
        let (c, _) = enc.encode_segment(&Segment { leaves: &bad, ..seg }).unwrap();
        assert_eq!(decode_value(&c, &natives()), invalid("output checksum mismatch"));
        // trees the spec does not allow
        for e in
            [entry(Some(5), "x", EntryNode::Dir), entry(None, "d", EntryNode::Dir), entry(None, "a/b", EntryNode::Dir)]
        {
            let mut t = tree.clone();
            t.entries.push(e);
            assert!(enc.encode_segment(&Segment { tree: &t, ..seg }).is_err());
        }
    }
}
