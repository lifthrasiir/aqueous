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
//!
//! The blocks of a batch alike (of a class and kind of stream) may take a dictionary trained
//! for them ([`super::dict`]), whose block comes before all of the batch's; a batch checked
//! alone has a stand-in for each dictionary of the batches before it, for the indices of its
//! own to be those of the segment.

use std::collections::{HashMap, HashSet};

use aqr_common::varint;

use super::dict::Candidate;
use super::tree::{self, FileParam, Tree};
use super::{
    CHECKSUM, Emit, Encoder, FOOTER_MAGIC, HEADER_MAGIC, How, MODULE, STORED, SWAP, always_compressed, block, block_of,
    block_ranges, checksum_of, dictionary_block, dictionary_stand_in, footer, footer_with, formula_block, key_how,
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
    Dictionary,
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
    dictionary_at: Vec<usize>,
    /// The dictionary (by index) of each leaf block taking one, by where it is.
    leaf_dictionary: HashMap<usize, usize>,
    /// The module and resource blocks so far, one after another, and a stand-in for each
    /// dictionary block ([`super::dictionary_stand_in`]).
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
    /// Where its dictionary blocks are in `bytes`, and the dictionary (by index in the
    /// segment) of each leaf taking one.
    dictionaries: Vec<usize>,
    leaf_dictionary: Vec<(usize, usize)>,
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
            dictionary_at: Vec::new(),
            leaf_dictionary: HashMap::new(),
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
        // (the streams are looked at in `f`: not copied)
        let check = Formula {
            streams: Vec::new(),
            root: Node::Concat(leaves.iter().flat_map(|l| l.outputs.iter().cloned()).collect()),
            shared: f.shared.clone(),
        };
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
        let todo: Vec<&crate::Stream> =
            (0..f.streams.len()).filter(|&st| written[st]).map(|st| &f.streams[st]).collect();
        enc.precompress(&todo)?;

        let level = enc.base_level();
        let mut b = Batch {
            bytes: Vec::new(),
            leaf_at: vec![0; leaves.len()],
            layout: Vec::new(),
            modules: Vec::new(),
            resources: Vec::new(),
            added: Vec::new(),
            dictionaries: Vec::new(),
            leaf_dictionary: Vec::new(),
        };
        let (mut modules, mut resources): (Vec<Module>, Vec<Resource>) = (self.modules.clone(), self.resources.clone());
        // (the non-leaf blocks counted from the batch's first: only their deltas are stored)
        let mut ordinals = vec![None; f.streams.len()];
        let mut non_leaf = 0u64;
        let mut done = vec![false; leaves.len()];
        let out = &mut b.bytes;

        // the dictionaries of the blocks alike, before all of the batch's blocks
        let mut whole = vec![false; f.streams.len()];
        (0..leaves.len()).for_each(|i| {
            if let Some(Some(st)) = data_leaf(i) {
                whole[st] = true;
            }
        });
        let mut candidates = Vec::new();
        for (st, s) in f.streams.iter().enumerate().filter(|&(st, _)| written[st]) {
            let (class, how) = ((&*s.class, s.kind), enc.how(s));
            match whole[st] {
                true => candidates.push(Candidate { class, data: &s.data, how }),
                false => {
                    candidates.extend(block_ranges(s.data.len()).map(|r| Candidate { class, data: &s.data[r], how }))
                }
            }
        }
        let planned = enc.dictionaries(&candidates)?;
        drop(candidates);
        let first_dictionary = self.dictionary_at.len();
        for d in &planned.blocks {
            let at = out.len();
            dictionary_block(out, d);
            b.layout.push((Placed::Dictionary, out.len() - at));
            b.dictionaries.push(at);
        }
        let dicts = (!planned.is_empty()).then_some((&planned, first_dictionary));

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
                        block(out, MODULE, &enc.module_block(m, level)?);
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
            let mut blocks = 0;
            for &st in &streams {
                ordinals[st] = Some(non_leaf + blocks);
                blocks += super::blocks_in(f.streams[st].data.len());
            }
            let refs: Vec<&Resource> = resources.iter().collect();
            for &i in &members {
                done[i] = true;
                let at = out.len();
                b.leaf_at[i] = self.at + at;
                match data_leaf(i) {
                    Some(stream) => {
                        let data: &[u8] = stream.map_or(&[], |st| &f.streams[st].data);
                        let how = stream.map_or(How::Usual, |st| enc.how(&f.streams[st]));
                        if let Some(&(d, _)) = planned.used.get(&key_how(&[data], how)) {
                            b.leaf_dictionary.push((i, first_dictionary + d));
                        }
                        enc.content_block(out, true, &[data], how, dicts)?;
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
                enc.stream_blocks(out, &[&f.streams[st].data], enc.how(&f.streams[st]), dicts)?;
                b.layout.push((Placed::NonLeaf(st), out.len() - at));
            }
            non_leaf += blocks;
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

    /// [`SegmentWriter::standalone`], of the bytes of `b` taken rather than copied: they are
    /// back in `b` once the archive is given to [`SegmentWriter::give_back`].
    pub fn take_standalone(&self, b: &mut Batch) -> Vec<u8> {
        let front = HEADER_MAGIC.len() + self.prefix.len();
        let mut out = std::mem::take(&mut b.bytes);
        let n = out.len();
        out.reserve(front + 32);
        out.resize(n + front, 0);
        out.copy_within(..n, front);
        out[..HEADER_MAGIC.len()].copy_from_slice(&HEADER_MAGIC);
        out[HEADER_MAGIC.len()..front].copy_from_slice(&self.prefix);
        let blocks = out.len() - HEADER_MAGIC.len();
        footer(&mut out, blocks);
        out
    }

    /// The bytes of `b` back from `archive`, its [`SegmentWriter::take_standalone`].
    pub fn give_back(&self, b: &mut Batch, mut archive: Vec<u8>) {
        let front = HEADER_MAGIC.len() + self.prefix.len();
        // (the footer's fields are as long as its last two bytes tell)
        let fields = u16::from_le_bytes(archive[archive.len() - 2..].try_into().unwrap()) as usize;
        let n = archive.len() - front - (FOOTER_MAGIC.len() + 4 + fields);
        archive.copy_within(front..front + n, 0);
        archive.truncate(n);
        b.bytes = archive;
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
        for &at in &b.dictionaries {
            self.dictionary_at.push(self.at + at);
            dictionary_stand_in(&mut self.prefix);
        }
        for &(i, d) in &b.leaf_dictionary {
            self.leaf_dictionary.insert(b.leaf_at[i], d);
        }
        self.at += b.bytes.len();
    }

    /// The end of the archive: the metadata block of `t`, its contents where `contents` tells
    /// (by [`EntryNode::File`]), and the footer. Fails on a tree the spec does not allow.
    pub fn finish(&self, enc: &Encoder, t: &Tree, contents: &[ContentAt]) -> Result<(Vec<u8>, usize)> {
        tree::check(t, contents.len()).map_err(Error::new)?;
        let params: Vec<FileParam> = contents
            .iter()
            .map(|c| {
                let dictionary = self.leaf_dictionary.get(&c.leaf).copied();
                FileParam { leaf: c.leaf, output: c.output as u64, size: c.size, dictionary }
            })
            .collect();
        let meta = tree::metadata(t, self.at, &self.module_at, &self.resource_at, &self.dictionary_at, &params);
        let mut out = Vec::new();
        match enc.base_level() {
            STORED => {
                let sum = checksum_of(&[&meta]).to_le_bytes();
                block_of(&mut out, METADATA_RAW, &[&meta, &sum]);
            }
            level => block(&mut out, METADATA_ZSTD, &always_compressed(&mut enc.cctx(), &[&meta], level, enc.threads)?),
        }
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
        // the blocks it slices, by their ordinals' deltas, and its own index of each stream's
        // first (a stream's blocks one after another)
        let mut sliced: Vec<(u64, usize)> = u.streams.iter().map(|&st| (ordinals[st].unwrap(), st)).collect();
        sliced.sort_unstable();
        let mut local = vec![None; f.streams.len()];
        let all: u64 = sliced.iter().map(|&(_, st)| super::blocks_in(f.streams[st].data.len())).sum();
        varint::put(&mut data, all);
        let (mut next, mut k) = (first, 0);
        for &(o, st) in &sliced {
            local[st] = Some(k);
            for b in 0..super::blocks_in(f.streams[st].data.len()) {
                varint::put(&mut data, o + b - next);
                next = o + b + 1;
                k += 1;
            }
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
            e.shared(i, &f.shared[i]);
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

/// A metadata block, uncompressed (at [`super::Level::STORED`]) or Zstandard compressed, with a u8
/// size field.
const METADATA_RAW: u64 = 0x10;
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
    use tree::{Entry, EntryNode, Extra, Time};

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
            let mut batch = w.batch(&enc, &f, &leaves).unwrap();
            let want = if at.is_empty() { &b"zyxolleh"[..] } else { b"ollehzyx" };
            assert_eq!(decode(&w.standalone(&batch), &natives()).unwrap(), want);
            // (alike of the batch's bytes taken, which are given back)
            let (bytes, alone) = (batch.bytes.clone(), w.standalone(&batch));
            let c = w.take_standalone(&mut batch);
            assert!(c == alone && batch.bytes.is_empty());
            w.give_back(&mut batch, c);
            assert!(batch.bytes == bytes);
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

    /// A segment of two batches of many small streams alike, of text records of the same
    /// words, its leaves' outputs one after another, and the dictionaries of each batch.
    fn alike(enc: &Encoder) -> (Vec<u8>, Vec<Vec<u8>>, Vec<usize>) {
        let mut rng = aqr_common::rng::Rng::new(7);
        let words: Vec<Vec<u8>> =
            (0..300).map(|_| (0..3 + rng.below(6)).map(|_| b'a' + rng.below(26) as u8).collect()).collect();
        let mut text = |n: usize| {
            let mut t = Vec::new();
            while t.len() < n {
                let w = |rng: &mut aqr_common::rng::Rng| &words[rng.below(words.len() as u64) as usize];
                t.extend_from_slice(format!("{{\"id\": {}, \"name\": \"", rng.below(100_000)).as_bytes());
                t.extend_from_slice(w(&mut rng));
                t.extend_from_slice(b"\", \"tags\": [\"");
                t.extend_from_slice(w(&mut rng));
                t.extend_from_slice(b"\"], \"note\": \"");
                for _ in 0..4 {
                    t.extend_from_slice(w(&mut rng));
                    t.push(b' ');
                }
                t.extend_from_slice(b"\"}\n");
            }
            t
        };
        let mut f = Formula::default();
        let mut w = SegmentWriter::new();
        let mut out = SegmentWriter::header().to_vec();
        let (mut contents, mut want, mut dictionaries) = (Vec::new(), Vec::new(), Vec::new());
        for _ in 0..2 {
            let mut leaves = Vec::new();
            // streams of a leaf of their own (leaf data blocks), and streams of two leaves
            for _ in 0..24 {
                let d = text(12 << 10);
                want.push(d.clone());
                leaves.push(Leaf { outputs: vec![f.push("t", d)], sums: vec![None] });
            }
            for _ in 0..8 {
                let d = text(12 << 10);
                let (st, half) = (f.streams.len(), d.len() as u64 / 2);
                want.push(d[..half as usize].to_vec());
                want.push(d[half as usize..].to_vec());
                f.push("t", d.clone());
                for (start, len) in [(0, half), (half, d.len() as u64 - half)] {
                    leaves.push(Leaf { outputs: vec![Node::slice(st, start, len)], sums: vec![None] });
                }
            }
            let batch = w.batch(enc, &f, &leaves).unwrap();
            let n = want.len() - leaves.len();
            assert_eq!(decode(&w.standalone(&batch), &natives()).unwrap(), want[n..].concat());
            dictionaries.push(batch.layout.iter().filter(|p| p.0 == Placed::Dictionary).count());
            w.commit(&batch);
            out.extend_from_slice(&batch.bytes);
            let sizes = leaves.iter().map(|l| l.outputs[0].len());
            contents.extend(batch.leaf_at.iter().zip(sizes).map(|(&leaf, size)| ContentAt { leaf, output: 0, size }));
        }
        let mut t = Tree::default();
        for c in 0..contents.len() {
            t.entries.push(entry(None, &format!("{c}"), EntryNode::File(c)));
        }
        out.extend_from_slice(&w.finish(enc, &t, &contents).unwrap().0);
        (out, want, dictionaries)
    }

    /// Streams alike, many and small, take a dictionary for each batch, as their leaf data
    /// blocks (the files' dictionaries in the metadata block) and non-leaf blocks do: the
    /// second batch's is the segment's second, as where its batch is checked alone (after a
    /// stand-in for the first). The archive is smaller than without dictionaries.
    #[test]
    fn dictionaries() {
        let enc = Encoder::new(Level::Zstd(3));
        let (c, want, dictionaries) = alike(&enc);
        assert_eq!(dictionaries, [1, 1]);
        let types: Vec<u64> = super::super::tests::types(&c).iter().map(|t| t & !3).collect();
        for ty in [super::super::DICTIONARY, super::super::LEAF_ZSTD_DICT, super::super::NON_LEAF_ZSTD_DICT] {
            assert!(types.contains(&ty), "{ty:#x}");
        }
        let Ok(Value::Tree(_, files)) = decode_value(&c, &natives()) else { panic!() };
        assert_eq!(files, want);
        let (without, again, none) = alike(&Encoder::new(Level::Zstd(3)).with_dictionaries(false));
        assert!(again == want && none == [0, 0]);
        assert!(c.len() * 10 < without.len() * 9, "{} against {}", c.len(), without.len());
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
        let cut = Apply {
            module: CUT,
            resources: vec![],
            params: vec![],
            inputs: vec![solid(2, 6)],
            len: 0,
            memory_limit: crate::NO_LIMIT,
        };
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
        tree.entries.push(entry(None, "fifo", EntryNode::Fifo));
        tree.entries.push(entry(None, "dev", EntryNode::Device { block: true, major: 1, minor: 1 << 40 }));
        let t = |secs, nanos| Some(Time { secs, nanos });
        tree.entries[2].extra = Extra {
            crtime: t(1_600_000_000, 0),
            ctime: t(-1, 999_999_999),
            mtime: t(1_700_000_000, 5),
            atime: t(1 << 40, 0),
            arctime: t(0, 1),
            link_group: Some(-1 << 64),
            xattrs: vec![(b"com.apple.quarantine".to_vec(), b"0081;".to_vec()), (b"\xff".to_vec(), vec![0; 300])],
            forks: vec![(b"AFP_Resource".to_vec(), 4), (b"\xed\xa0\x80".to_vec(), 0)],
            win_link_type: None,
            win_attrs: Some(0x2127),
            mode: Some(0o644),
            uid: Some(u64::MAX),
            gid: Some(20),
            uname: Some(b"\xc1".to_vec()),
            gname: Some("스태프".into()),
            mac_flags: Some(0x8000),
        };
        tree.entries[11].extra.mode = Some(0o600);
        tree.entries[9].extra.win_link_type = Some(1);
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
        let want_names: [&[u8]; 12] =
            [b"d", b"e", b"own", b"solid", b"empty", b"rev", b"cut0", b"cut1", b"same", b"link", b"fifo", b"dev"];
        assert_eq!(names, want_names);
        assert_eq!(t.entries[2].extra, tree.entries[2].extra);
        assert_eq!(t.entries[9].node, EntryNode::Link(b"../own".to_vec()));
        assert_eq!(t.entries[9].extra.win_link_type, Some(1));
        assert_eq!(t.entries[10].node, EntryNode::Fifo);
        assert_eq!(t.entries[11].node, tree.entries[11].node);
        assert_eq!(t.entries[11].extra, tree.entries[11].extra);
        assert_eq!(t.entries[9].parent, Some(1));
        let got: Vec<&[u8]> = t
            .entries
            .iter()
            .filter_map(|e| if let EntryNode::File(c) = e.node { Some(&files[c][..]) } else { None })
            .collect();
        assert_eq!(got, [want[0], want[1], want[2], want[3], want[4], want[5], want[3]]);
        assert_eq!(files.len(), 6);
        assert_eq!(decode(&c, &natives()), super::super::resource("an archive of several files"));
        // at level 0, of no compressed block but those that are always (formula and module
        // blocks), and the same files
        let (stored, _) = Encoder::new(Level::STORED).encode_segment(&seg).unwrap();
        let types: Vec<u64> = super::super::tests::types(&stored).iter().map(|t| t & !3).collect();
        assert!(types.contains(&METADATA_RAW) && !types.contains(&METADATA_ZSTD));
        assert!(!types.contains(&super::super::LEAF_ZSTD) && !types.contains(&super::super::NON_LEAF_ZSTD));
        let Ok(Value::Tree(_, again)) = decode_value(&stored, &natives()) else { panic!() };
        assert_eq!(again, files);

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
        // extras the spec does not allow
        let bad: [fn(&mut Extra); 6] = [
            |x| x.forks.push((b"AFP_Resource".to_vec(), 0)),
            |x| x.forks.push((Vec::new(), 0)),
            |x| x.forks[0].1 = 6,
            |x| x.xattrs.push((b"\xff".to_vec(), Vec::new())),
            |x| x.mac_flags = Some(0x20),
            |x| x.win_attrs = Some(0x10),
        ];
        for f in bad {
            let mut t = tree.clone();
            f(&mut t.entries[2].extra);
            assert!(enc.encode_segment(&Segment { tree: &t, ..seg }).is_err());
        }
        for (i, g) in [(0, 1), (2, 1 << 64)] {
            let mut t = tree.clone();
            t.entries[i].extra.link_group = Some(g);
            assert!(enc.encode_segment(&Segment { tree: &t, ..seg }).is_err());
        }
        for (i, l) in [(2, 1), (9, 3)] {
            let mut t = tree.clone();
            t.entries[i].extra.win_link_type = Some(l);
            assert!(enc.encode_segment(&Segment { tree: &t, ..seg }).is_err());
        }
    }
}
