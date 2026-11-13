//! An archive of several files, as one or more segments, written a batch of leaves at a time
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
use super::tree::{self, FileParam, Status, Tree};
use super::{
    CHECKSUM, Emit, Encoder, FOOTER_MAGIC, HEADER_MAGIC, How, STORED, SWAP, always_compressed, block, block_of,
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

/// The header of an archive with a size hint block to be filled in.
const HINTED: [u8; 18] = {
    let mut h = [0; 18];
    let mut i = 0;
    while i < 18 {
        h[i] = if i < 6 { HEADER_MAGIC[i] } else { super::SIZE_HINT_PLACEHOLDER[i - 6] };
        i += 1;
    }
    h
};

/// The blocks of an archive in order, each what it is and its bytes.
pub type Layout = Vec<(Placed, usize)>;

/// An archive being written, a segment at a time: where it is, and the modules and resources of
/// the blocks of its segment so far. A segment is closed by its metadata block
/// ([`SegmentWriter::close`]), the last by [`SegmentWriter::finish`] (and the footer); what each
/// tells is of its own blocks, each self-contained (spec, "Content blocks").
pub struct SegmentWriter {
    /// Bytes of the archive so far, its header magic among them.
    at: usize,
    /// Where the segment started (after the metadata block before it, or the header and size
    /// hint), and where the metadata block before it is (the end of the header magic, less 1,
    /// where there is none).
    segment_at: usize,
    prev: usize,
    /// Whether the archive has a size hint block ([`SegmentWriter::with_size_hint`]).
    hinted: bool,
    modules: Vec<Module>,
    module_at: Vec<usize>,
    resources: Vec<Resource>,
    resource_at: Vec<usize>,
    dictionary_at: Vec<usize>,
    /// Bytes of the blocks so far that may be mirrored but the metadata block (module,
    /// resource, dictionary and formula blocks): about those of their mirror blocks.
    mirrorable: usize,
    /// The dictionary (by index) of each leaf block taking one, by where it is.
    leaf_dictionary: HashMap<usize, usize>,
    /// The module and resource blocks so far, one after another, and a stand-in for each
    /// dictionary block ([`super::dictionary_stand_in`]).
    prefix: Vec<u8>,
    /// The footer of the last commit, where there is one ([`SegmentWriter::footer`]): to be made a
    /// spacer block by the next commit once it is written. Its padding byte, where its length is
    /// odd, is counted in `at` but not yet written where `pad`.
    live: Option<std::ops::Range<usize>>,
    pad: bool,
    /// The journal points the next metadata block tells ([`SegmentWriter::set_journal`]).
    journal: Vec<(u64, String)>,
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

/// A block copied into a segment ([`SegmentWriter::copied`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Copied {
    Module,
    Resource,
    Dictionary,
    /// A leaf block, and the dictionary it takes (by its index in the segment).
    Leaf(Option<usize>),
    NonLeaf,
}

/// Where a content of an archive is: the leaf block (where it starts in the archive), its output
/// and its size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContentAt {
    pub leaf: usize,
    pub output: usize,
    pub size: u64,
}

/// The bytes that make a footer of `len` bytes, and its padding byte where `len` is odd, a spacer
/// block once they are written over its first bytes (spec, "Appending to an archive"): its type and
/// size, 2 bytes for a footer of 257 bytes or less. The rest of the footer is to be zeroed after,
/// as a spacer block's space is.
pub fn kill(len: usize) -> Vec<u8> {
    let n = len + len % 2;
    if n - 2 <= 0xff {
        vec![0, (n - 2) as u8]
    } else if n - 3 <= 0xffff {
        [&[1u8][..], &((n - 3) as u16).to_le_bytes()].concat()
    } else {
        [&[2u8][..], &((n - 5) as u32).to_le_bytes()].concat()
    }
}

/// Whether `n` bytes written at offset `at` of a file cross a multiple of 512 bytes: a write that
/// storage devices may then do in part (spec, "Appending to an archive").
pub fn crossing(at: u64, n: usize) -> bool {
    at % 512 + n as u64 > 512
}

impl Default for SegmentWriter {
    fn default() -> SegmentWriter {
        SegmentWriter::new()
    }
}

impl SegmentWriter {
    /// A segment of no blocks yet, after the header ([`SegmentWriter::header`]).
    pub fn new() -> SegmentWriter {
        SegmentWriter::with_size_hint(false)
    }

    /// [`SegmentWriter::new`], its header followed by a size hint block to be filled in where
    /// `on` ([`super::SIZE_HINT_PLACEHOLDER`]), as an encrypted archive's payload has to.
    pub fn with_size_hint(on: bool) -> SegmentWriter {
        let at = if on { HINTED.len() } else { HEADER_MAGIC.len() };
        SegmentWriter {
            at,
            segment_at: at,
            prev: HEADER_MAGIC.len() - 1,
            hinted: on,
            modules: Vec::new(),
            module_at: Vec::new(),
            resources: Vec::new(),
            resource_at: Vec::new(),
            dictionary_at: Vec::new(),
            mirrorable: 0,
            leaf_dictionary: HashMap::new(),
            prefix: Vec::new(),
            live: None,
            pad: false,
            journal: Vec::new(),
        }
    }

    /// A segment to append to an archive whose footer is at `footer` (from its header magic),
    /// ending it, and whose last metadata block is at `metadata` (spec, "Appending to an
    /// archive"): its blocks after the footer (and a padding byte, [`SegmentWriter::pad`]), the
    /// footer to be made a spacer block by the first commit ([`SegmentWriter::footer`]).
    pub fn appending(footer: std::ops::Range<usize>, metadata: usize) -> SegmentWriter {
        let mut w = SegmentWriter::new();
        let odd = footer.len() % 2 == 1;
        w.at = footer.end + odd as usize;
        (w.segment_at, w.prev, w.live, w.pad) = (w.at, metadata, Some(footer), odd);
        w
    }

    /// A segment to append to an archive whose footer, at `footer` (from its header magic) and
    /// ending it, cannot be made a spacer block in place (spec, "Appending to an archive": its
    /// first bytes across a sector of the file): written over, from where it starts, by the
    /// segment's blocks; the archive without a footer until the first commit.
    pub fn replacing(footer: usize, metadata: usize) -> SegmentWriter {
        let mut w = SegmentWriter::new();
        (w.at, w.segment_at, w.prev) = (footer, footer, metadata);
        w
    }

    /// Has the metadata block of the segment being written tell `points` as its `journal` (spec,
    /// "`journal`"): times in milliseconds since the epoch (UTC) and labels, of times not repeated.
    /// Taken by [`SegmentWriter::close_as`]; [`SegmentWriter::end`] and [`SegmentWriter::finish_as`]
    /// tell them without taking.
    pub fn set_journal(&mut self, mut points: Vec<(u64, String)>) -> Result<()> {
        points.sort_by_key(|p| p.0);
        if points.windows(2).any(|w| w[0].0 == w[1].0) {
            return Err(Error::new("a time repeated in the journal"));
        }
        self.journal = points;
        Ok(())
    }

    /// Whether the segment being written is the archive's first, which has to have an entry
    /// (spec, "Metadata block"): a tree the spec allows.
    fn check(&self, t: &Tree, contents: usize) -> Result<()> {
        if self.prev == HEADER_MAGIC.len() - 1 && t.entries.is_empty() {
            return Err(Error::new("the first segment of no entries"));
        }
        tree::check(t, contents).map_err(Error::new)
    }

    /// The footer of the archive so far, to write after the metadata block of the segment just
    /// closed ([`SegmentWriter::close_as`]), committing what is before it (spec, "Appending to an
    /// archive"): the bytes to write (a spacer block first where the footer would cross a sector
    /// of the file, the archive starting at `base` in it, [`crossing`]), and the footer of the
    /// commit before, to make a spacer block of ([`kill`]) once they are durably written.
    pub fn footer(&mut self, base: u64) -> (Vec<u8>, Option<std::ops::Range<usize>>) {
        let (out, start) = self.placed_footer(self.at, self.prev, base);
        let len = out.len() - (start - self.at);
        let odd = len % 2 == 1;
        self.at = start + len + odd as usize;
        self.segment_at = self.at;
        self.pad = odd;
        (out, self.live.replace(start..start + len))
    }

    /// The end of the archive, of the segment so far (as [`SegmentWriter::close_as`] closes it)
    /// committed ([`SegmentWriter::footer`]), without closing it: the metadata block of `t`
    /// telling `status` and the footer, to write after the blocks so far (and the padding byte
    /// due, [`SegmentWriter::pad_due`]), and the footer of the commit before.
    pub fn end(
        &self,
        enc: &Encoder,
        t: &Tree,
        contents: &[ContentAt],
        status: Status,
        base: u64,
    ) -> Result<(Vec<u8>, Option<std::ops::Range<usize>>)> {
        self.check(t, contents.len())?;
        if contents.iter().any(|c| c.leaf < self.segment_at) {
            return Err(Error::new("a content of a leaf of another segment"));
        }
        let mut out = self.metadata_block(enc, t, contents, usize::MAX, 0, status)?;
        let (footer, _) = self.placed_footer(self.at + out.len(), self.at, base);
        out.extend_from_slice(&footer);
        Ok((out, self.live.clone()))
    }

    /// A footer to write at `at`, of an archive whose last metadata block is at `metadata`,
    /// placed off the sectors of the file (the archive starting at `base` in it): a spacer block
    /// first where it has to be moved, and where the footer then starts.
    fn placed_footer(&self, at: usize, metadata: usize, base: u64) -> (Vec<u8>, usize) {
        let mut out = Vec::new();
        let mut f = Vec::new();
        let mut room = 0;
        loop {
            f.clear();
            let start = at + room;
            // (from the start of the footer's fields: after the magic and the size)
            footer_with(&mut f, start - HEADER_MAGIC.len(), (start + FOOTER_MAGIC.len() + 2 - metadata) as u64);
            if !crossing(base + start as u64, kill(f.len()).len()) {
                break;
            }
            room += 2;
        }
        if room > 0 {
            out.extend_from_slice(&[0, (room - 2) as u8]);
            out.resize(room, 0);
        }
        out.extend_from_slice(&f);
        (out, at + room)
    }

    /// Whether the padding byte after the footer of a commit is still to be written
    /// ([`SegmentWriter::pad`]).
    pub fn pad_due(&self) -> bool {
        self.pad
    }

    /// The padding byte to write before anything after the footer of a commit, once
    /// ([`SegmentWriter::footer`]): empty where there is none, or it was taken.
    pub fn pad(&mut self) -> &'static [u8] {
        match std::mem::take(&mut self.pad) {
            true => &[0],
            false => &[],
        }
    }

    /// The bytes of the archive before its blocks (but a size hint block).
    pub fn header() -> &'static [u8] {
        &HEADER_MAGIC
    }

    /// The bytes of the archive before the blocks of its batches: the header, and the size
    /// hint block where there is one.
    pub fn head(&self) -> &'static [u8] {
        if self.hinted { &HINTED } else { &HEADER_MAGIC }
    }

    /// Bytes of the archive so far.
    pub fn len(&self) -> usize {
        self.at
    }

    pub fn is_empty(&self) -> bool {
        self.at == self.head().len()
    }

    /// Bytes of the segment so far.
    pub fn segment_len(&self) -> usize {
        self.at - self.segment_at
    }

    /// Whether the segment has no blocks yet.
    pub fn segment_is_empty(&self) -> bool {
        self.at == self.segment_at
    }

    /// Bytes of the blocks of the segment so far that may be mirrored (module, resource,
    /// dictionary and formula blocks): about those of their mirror blocks.
    pub fn mirrorable(&self) -> usize {
        self.mirrorable
    }

    /// Takes `n` bytes written after the blocks so far that are of no segment (mirror and
    /// spacer blocks, [`super::Mirroring::mirror_before`]) as written.
    pub fn skip(&mut self, n: usize) {
        self.at += n;
    }

    /// Takes a block of `n` bytes written after the blocks so far, as it was in another archive
    /// ([`super::rewrite`]), as one of the segment's: what it is for the metadata block. A copied
    /// segment takes no batch: its modules and resources are not known.
    pub(super) fn copied(&mut self, what: Copied, n: usize) {
        match what {
            Copied::Module => self.module_at.push(self.at),
            Copied::Resource => self.resource_at.push(self.at),
            Copied::Dictionary => self.dictionary_at.push(self.at),
            Copied::Leaf(Some(d)) => _ = self.leaf_dictionary.insert(self.at, d),
            Copied::Leaf(None) | Copied::NonLeaf => {}
        }
        self.at += n;
    }

    /// Closes the segment with the metadata block of `t`, its contents where `contents` tells
    /// (by [`EntryNode::File`], of leaves of the segment): the bytes to write after its blocks.
    /// The blocks after it are of a segment of their own, which takes nothing of this one's.
    /// Fails on a tree the spec does not allow.
    ///
    /// [`EntryNode::File`]: tree::EntryNode::File
    pub fn close(&mut self, enc: &Encoder, t: &Tree, contents: &[ContentAt]) -> Result<Vec<u8>> {
        self.close_as(enc, t, contents, Status::None)
    }

    /// [`SegmentWriter::close`], the metadata block telling `status` (`segment-status`).
    pub fn close_as(&mut self, enc: &Encoder, t: &Tree, contents: &[ContentAt], status: Status) -> Result<Vec<u8>> {
        self.check(t, contents.len())?;
        if contents.iter().any(|c| c.leaf < self.segment_at) {
            return Err(Error::new("a content of a leaf of another segment"));
        }
        let out = self.metadata_block(enc, t, contents, usize::MAX, 0, status)?;
        self.journal.clear();
        self.prev = self.at;
        self.at += out.len();
        self.segment_at = self.at;
        self.modules.clear();
        self.module_at.clear();
        self.resources.clear();
        self.resource_at.clear();
        self.dictionary_at.clear();
        self.mirrorable = 0;
        self.leaf_dictionary.clear();
        self.prefix.clear();
        Ok(out)
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
        // (a group's root is its least leaf)
        let mut groups: Vec<Vec<usize>> = vec![Vec::new(); leaves.len()];
        for (i, &root) in roots.iter().enumerate() {
            groups[root].push(i);
        }

        // the streams compressed at once, then laid out
        let written = written(f, &uses);
        enc.precompress(&(0..f.streams.len()).filter(|&st| written[st]).map(|st| &f.streams[st]).collect::<Vec<_>>())?;

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
            if roots[first] != first {
                continue;
            }
            let members = &groups[first];
            // the modules and resources of the group not written yet
            for &i in members {
                for (m, d) in &uses[i].module_bytes {
                    enc.check_module_bytes(*m, d, level)?;
                }
                for &m in &uses[i].modules {
                    if !modules.contains(&m) {
                        let at = out.len();
                        enc.write_module(out, m, level)?;
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
            for &i in members {
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
        self.mirrorable += (b.layout.iter())
            .filter(|(p, _)| matches!(p, Placed::Module | Placed::Resource | Placed::Dictionary | Placed::Formula(_)))
            .map(|&(_, n)| n)
            .sum::<usize>();
    }

    /// The end of the archive: the metadata block of `t`, its contents where `contents` tells
    /// (by [`EntryNode::File`]), and the footer. Fails on a tree the spec does not allow.
    ///
    /// [`EntryNode::File`]: tree::EntryNode::File
    pub fn finish(&self, enc: &Encoder, t: &Tree, contents: &[ContentAt]) -> Result<(Vec<u8>, usize)> {
        self.finish_as(enc, t, contents, Status::Complete)
    }

    /// [`SegmentWriter::finish`], the metadata block telling `status` (`segment-status`).
    pub fn finish_as(
        &self,
        enc: &Encoder,
        t: &Tree,
        contents: &[ContentAt],
        status: Status,
    ) -> Result<(Vec<u8>, usize)> {
        self.check(t, contents.len())?;
        let mut out = self.metadata_block(enc, t, contents, usize::MAX, 0, status)?;
        let metadata = out.len();
        let end = self.at + out.len();
        // (from the start of the footer's fields: after the magic and the size)
        let back = end + FOOTER_MAGIC.len() + 2 - self.at;
        footer_with(&mut out, end - HEADER_MAGIC.len(), back as u64);
        Ok((out, metadata))
    }

    /// [`SegmentWriter::finish`] with a mirror block of the metadata block before the blocks
    /// written from `front` on (held back, as they are to come after it), so that a truncation
    /// of the archive shorter than they leaves it: the bytes to write at `front` (the mirror
    /// block, then a spacer block where the room it was given is longer), and those to write
    /// after the blocks held back. The room is found as the metadata block, whose offsets to the
    /// blocks before `front` count it, is made again until its mirror block fits. None where the
    /// mirror block would be nearer the metadata block than the spec asks; but in an encrypted
    /// archive's payload (`chunked`), where the room has to come out exact (it may have no
    /// spacer blocks), it is put there however near where the blocks held back are more than
    /// the other blocks' mirror blocks after the metadata block would be.
    pub fn finish_mirrored(
        &self,
        enc: &Encoder,
        t: &Tree,
        contents: &[ContentAt],
        front: usize,
        chunked: bool,
        status: Status,
    ) -> Result<Option<(Vec<u8>, Vec<u8>)>> {
        self.check(t, contents.len())?;
        let mut room = 0;
        for tries in 0..8 {
            let meta = self.metadata_block(enc, t, contents, front, room, status)?;
            let at = self.at + room;
            let mut mirror = super::mirror::mirror_of(&meta, at as u64, front as u64, false);
            let len = mirror.len();
            if len > room || (chunked && len != room) {
                // (a little more each time, where it does not settle)
                room = len + if tries < 3 { 0 } else { 2 << tries };
                continue;
            }
            // (in an encrypted archive's payload, before the blocks however near, where that is
            // further than after them all, the other mirror blocks alone between)
            let near = super::mirror::least_start((front + len) as u64, chunked) > at as u64;
            if near && (!chunked || self.at - front < self.mirrorable) {
                return Ok(None);
            }
            if room > len {
                super::mirror::spacer(&mut mirror, (room - len) as u64);
            }
            let mut end = meta;
            let e = at + end.len();
            footer_with(&mut end, e - HEADER_MAGIC.len(), (e + FOOTER_MAGIC.len() + 2 - at) as u64);
            return Ok(Some((mirror, end)));
        }
        Ok(None)
    }

    /// The metadata block of `t` (its contents where `contents` tells), the blocks from `front`
    /// on (and the metadata block, after them all) moved `shift` bytes later, telling `status`
    /// (`segment-status`).
    fn metadata_block(
        &self,
        enc: &Encoder,
        t: &Tree,
        contents: &[ContentAt],
        front: usize,
        shift: usize,
        status: Status,
    ) -> Result<Vec<u8>> {
        let moved = |at: usize| if at >= front { at + shift } else { at };
        let params: Vec<FileParam> = contents
            .iter()
            .map(|c| {
                let dictionary = self.leaf_dictionary.get(&c.leaf).copied();
                FileParam { leaf: moved(c.leaf), output: c.output as u64, size: c.size, dictionary }
            })
            .collect();
        let all = |v: &[usize]| v.iter().map(|&a| moved(a)).collect::<Vec<_>>();
        let (modules, resources, dictionaries) =
            (all(&self.module_at), all(&self.resource_at), all(&self.dictionary_at));
        let meta = tree::metadata(
            t,
            self.at + shift,
            moved(self.prev),
            status,
            &self.journal,
            &modules,
            &resources,
            &dictionaries,
            &params,
        );
        let mut out = Vec::new();
        match enc.base_level() {
            STORED => {
                let sum = checksum_of(&[&meta]).to_le_bytes();
                block_of(&mut out, METADATA_RAW, &[&meta, &sum]);
            }
            level => block(&mut out, METADATA_ZSTD, &always_compressed(&mut enc.cctx(), &[&meta], level, enc.threads)?),
        }
        Ok(out)
    }
}

impl Encoder {
    /// The archive of segment `s`, with what each of its blocks is: all of its leaves one batch.
    /// Fails on slices out of their streams, on outputs not taken once each (by one leaf), and
    /// on a tree the spec does not allow.
    pub fn encode_segment(&self, s: &Segment) -> Result<(Vec<u8>, Layout)> {
        if s.tree.entries.is_empty() {
            return Err(Error::new("the first segment of no entries"));
        }
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
        // the blocks it slices (not every block of a stream it slices: the spec wants each block
        // listed used), by their ordinals' deltas, and its own index of each
        let mut sliced: Vec<(u64, usize)> = u.streams.iter().map(|&st| (ordinals[st].unwrap(), st)).collect();
        sliced.sort_unstable();
        let mut nodes: Vec<&Node> = u.shared.iter().flat_map(|&i| f.shared[i].inputs()).collect();
        nodes.extend(&leaf.outputs);
        let used = super::sliced(f.streams.len(), |st| f.streams[st].data.len(), nodes);
        let (local, all) = super::listed(&used, sliced.iter().map(|&(_, st)| st));
        varint::put(&mut data, all);
        let mut next = first;
        for &(o, st) in &sliced {
            for (b, _) in used[st].iter().enumerate().filter(|(_, u)| **u) {
                varint::put(&mut data, o + b as u64 - next);
                next = o + b as u64 + 1;
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
        let classes = self.classes_of(&u.modules);
        let mut e = Emit {
            ordinals: &local,
            modules: &u.modules,
            classes: &classes,
            resources: &own_resources,
            out: &mut data,
            stack: vec![],
        };
        for &i in &u.shared {
            e.shared(i, &f.shared[i]);
        }
        for (k, n) in leaf.outputs.iter().enumerate() {
            e.node(n);
            e.sink(k);
        }
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
/// resources of its applications and of the bytes of blocks it takes, in the order of their
/// first; and the bytes it takes of module blocks ([`Node::ModuleBytes`]).
struct Uses {
    streams: Vec<usize>,
    shared: Vec<usize>,
    modules: Vec<Module>,
    resources: Vec<Resource>,
    module_bytes: Vec<(Module, std::sync::Arc<[u8]>)>,
}

/// Of each stream of `f`, whether leaves of `uses` take it.
fn written(f: &Formula, uses: &[Uses]) -> Vec<bool> {
    let mut written = vec![false; f.streams.len()];
    uses.iter().for_each(|u| u.streams.iter().for_each(|&st| written[st] = true));
    written
}

/// Compresses the streams of `f` that [`SegmentWriter::batch`] writes of `leaves`, as it does
/// ([`Encoder::precompress`]), beforehand: a batch made ready while another is written.
pub fn precompress_leaves(enc: &Encoder, f: &Formula, leaves: &[Leaf]) -> Result<()> {
    let uses: Vec<Uses> = leaves.iter().map(|l| uses(f, l)).collect();
    let written = written(f, &uses);
    enc.precompress(&(0..f.streams.len()).filter(|&st| written[st]).map(|st| &f.streams[st]).collect::<Vec<_>>())
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
            f.shared[i].inputs().iter().for_each(|n| mark(n, &mut shared));
        }
    }
    let shared: Vec<usize> = (0..f.shared.len()).filter(|&i| shared[i]).collect();
    let mut u = Uses {
        streams: Vec::new(),
        shared: Vec::new(),
        modules: Vec::new(),
        resources: Vec::new(),
        module_bytes: Vec::new(),
    };
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
            Node::ModuleBytes(m, d) => {
                if !u.modules.contains(m) {
                    u.modules.push(*m);
                }
                u.module_bytes.push((*m, d.clone()));
            }
            Node::ResourceBytes(r) if !u.resources.contains(r) => u.resources.push(r.clone()),
            _ => {}
        })
    }
    // (the applications of several outputs first, as they are emitted)
    for &i in &shared {
        f.shared[i].inputs().iter().for_each(|n| visit(&mut u, n));
        f.shared[i].apply().into_iter().for_each(|a| note(&mut u, a));
    }
    leaf.outputs.iter().for_each(|n| visit(&mut u, n));
    u.shared = shared;
    u
}

#[cfg(test)]
mod tests {
    use super::super::tests::{CUT, REV, natives};
    use super::super::{
        Archive, BLOCK, Level, Payload, Value, checksum, decode, decode_value, invalid, leb, locate, open,
        open_indexed, open_indexed_upto, open_upto,
    };
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

    /// Contents taken alone evaluate only their leaves, and decode only the non-leaf blocks those
    /// slice: of three leaves in batches of their own, the second.
    #[test]
    fn contents_of() {
        let mut f = Formula::default();
        let names = ["hello", "xyz", "uvw"];
        let lits: Vec<_> = names.iter().map(|s| f.push(*s, s.as_bytes().to_vec())).collect();
        let enc = Encoder::new(Level::Zstd(3));
        let mut w = SegmentWriter::new();
        let mut out = SegmentWriter::header().to_vec();
        let mut t = Tree::default();
        let mut contents = Vec::new();
        for (k, a) in lits.iter().enumerate() {
            let leaves = vec![Leaf {
                outputs: vec![Node::apply(REV, vec![1, 2, 3], vec![a.clone()], names[k].len() as u64)],
                sums: vec![None],
            }];
            let batch = w.batch(&enc, &f, &leaves).unwrap();
            w.commit(&batch);
            out.extend_from_slice(&batch.bytes);
            contents.push(ContentAt { leaf: batch.leaf_at[0], output: 0, size: names[k].len() as u64 });
            t.entries.push(entry(None, &format!("f{k}"), EntryNode::File(k)));
        }
        out.extend_from_slice(&w.finish(&enc, &t, &contents).unwrap().0);
        let natives = natives();
        let mut a = super::super::open(&out, &natives).unwrap();
        let told = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let tell = told.clone();
        a.on_progress(Some(std::sync::Arc::new(move |s| tell.lock().unwrap().push(s))));
        let got = std::sync::Mutex::new(Vec::new());
        a.contents_of(2, Some(&[false, true, false]), |c, d| {
            got.lock().unwrap().push((c, d));
            Ok(())
        })
        .unwrap();
        assert_eq!(got.into_inner().unwrap(), [(1, b"zyx".to_vec())]);
        let told = told.lock().unwrap();
        let started: Vec<usize> =
            told.iter().filter_map(|s| if let super::super::Step::Started(i) = s { Some(*i) } else { None }).collect();
        assert_eq!(started, [1]);
        // (the data of the second batch's blocks alone counted, and decoded)
        let reached = told.iter().filter_map(|s| if let super::super::Step::Reached(r) = s { Some(*r) } else { None });
        let most = reached.fold((0, 0, 0, 0), |m, r| {
            (m.0.max(r.decompressed.0), r.decompressed.1, m.2.max(r.evaluated.0), r.evaluated.1)
        });
        assert!(most.0 == most.1 && most.1 > 0 && (most.2, most.3) == (3, 3), "{most:?}");
        // (fewer bytes than of the whole archive's blocks)
        let mut all = super::super::open(&out, &natives).unwrap();
        let whole = std::sync::Arc::new(std::sync::Mutex::new(0));
        let w = whole.clone();
        all.on_progress(Some(std::sync::Arc::new(move |s| {
            if let super::super::Step::Reached(r) = s {
                *w.lock().unwrap() = r.decompressed.1;
            }
        })));
        all.contents(2, |_, _| Ok(())).unwrap();
        assert!(most.1 < *whole.lock().unwrap());
        // opened by its index: the same, alone and all of them
        let a = super::super::open_indexed(super::super::Payload::Plain(&out), &natives, 2).unwrap();
        let got = std::sync::Mutex::new(Vec::new());
        a.contents_of(2, Some(&[false, true, false]), |c, d| {
            got.lock().unwrap().push((c, d));
            Ok(())
        })
        .unwrap();
        assert_eq!(got.into_inner().unwrap(), [(1, b"zyx".to_vec())]);
        let got = std::sync::Mutex::new(vec![Vec::new(); 3]);
        a.contents(2, |c, d| {
            got.lock().unwrap()[c] = d;
            Ok(())
        })
        .unwrap();
        assert_eq!(got.into_inner().unwrap(), [b"olleh".to_vec(), b"zyx".to_vec(), b"wvu".to_vec()]);
    }

    /// The bytes of module and resource blocks in a batch: of a module applied by another leaf
    /// and of one only taken so, and of a resource as stored.
    #[test]
    fn block_bytes() {
        let mut f = Formula::default();
        let a = f.push("a", b"hello".to_vec());
        let enc = Encoder::new(Level::Zstd(3));
        let block_of = |m| -> std::sync::Arc<[u8]> {
            let w = crate::aqr::stand_in(m);
            crate::aqr::always_compressed(&mut zstd::zstd_safe::CCtx::create(), &[&w], 3, 1).unwrap().into()
        };
        let (rev, cut) = (block_of(REV), block_of(CUT));
        let stored = crate::aqr::always_compressed(&mut zstd::zstd_safe::CCtx::create(), &[b"model"], 3, 1).unwrap();
        let r = Resource::stored(stored.clone()).unwrap();
        let leaves = vec![
            Leaf { outputs: vec![Node::apply(REV, vec![1, 2, 3], vec![a.clone()], 5)], sums: vec![None] },
            Leaf {
                outputs: vec![Node::concat([
                    a,
                    Node::ModuleBytes(REV, rev.clone()),
                    Node::ModuleBytes(CUT, cut.clone()),
                    Node::ResourceBytes(r),
                ])],
                sums: vec![None],
            },
        ];
        let w = SegmentWriter::new();
        let batch = w.batch(&enc, &f, &leaves).unwrap();
        assert_eq!(batch.layout.iter().filter(|p| p.0 == Placed::Module).count(), 2);
        let want = [&b"olleh"[..], b"hello", &rev, &cut, &stored].concat();
        assert_eq!(decode(&w.standalone(&batch), &natives()).unwrap(), want);
        // a module's block of other bytes
        let mut other = leaves;
        other[1].outputs = vec![Node::ModuleBytes(REV, cut)];
        assert!(w.batch(&enc, &f, &other).is_err());
    }

    /// Leaves slicing parts of a stream of several blocks list only the blocks they slice: one
    /// leaf of all of it and others of a block or two, as files shared within a tar are.
    #[test]
    fn leaves_of_some_blocks() {
        let mut f = Formula::default();
        let n = 3 * BLOCK + 100;
        let d: Vec<u8> = (0..n as u32).map(|i| (i.wrapping_mul(2654435761) >> 24) as u8).collect();
        f.push("a", d.clone());
        let b = BLOCK as u64;
        let parts = [(0, n as u64), (10, 20), (2 * b + 5, 50), (b - 5, 10), (3 * b, 100)];
        let leaves: Vec<Leaf> =
            parts.iter().map(|&(s, l)| Leaf { outputs: vec![Node::slice(0, s, l)], sums: vec![None] }).collect();
        let want: Vec<u8> = parts.iter().flat_map(|&(s, l)| d[s as usize..][..l as usize].to_vec()).collect();
        let w = SegmentWriter::new();
        let batch = w.batch(&Encoder::new(Level::Zstd(1)), &f, &leaves).unwrap();
        assert!(decode(&w.standalone(&batch), &natives()).unwrap() == want);
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
            task_limits: Vec::new(),
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
        let t = |secs, nanos| Some(Time::new(secs, nanos));
        tree.entries[2].extra = Extra {
            crtime: t(1_600_000_000, 0),
            ctime: t(-1, 999_999_999),
            mtime: t(1_700_000_000, 5),
            atime: t(1 << 40, 0),
            arctime: t(0, 1),
            link_group: Some(-1 << 64),
            xattrs: vec![
                (b"com.apple.quarantine"[..].into(), b"0081;"[..].into()),
                (b"\xff"[..].into(), vec![0; 300].into()),
            ],
            forks: vec![(b"AFP_Resource".to_vec(), 4), (b"\xed\xa0\x80".to_vec(), 0)],
            win_link_type: None,
            win_attrs: Some(0x2127),
            mode: Some(0o644),
            uid: Some(u64::MAX),
            gid: Some(20),
            uname: Some(b"\xc1"[..].into()),
            gname: Some("스태프".as_bytes().into()),
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
        // (a time beyond 64 bits of nanoseconds of seconds)
        let atime = Some(Time { unit: 1_000_000_000, ..Time::new(1 << 40, 0) });
        assert_eq!(t.entries[2].extra, Extra { atime, ..tree.entries[2].extra.clone() });
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
            |x| x.xattrs.push((b"\xff"[..].into(), b""[..].into())),
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

    /// The archive of `out` with the footer at `old` made a spacer block (spec, "Appending to an
    /// archive").
    fn killed(out: &[u8], old: std::ops::Range<usize>) -> Vec<u8> {
        let mut out = out.to_vec();
        let k = kill(old.len());
        out[old.start..old.start + k.len()].copy_from_slice(&k);
        out[old.start + k.len()..old.end].fill(0);
        out
    }

    #[test]
    fn commits() {
        let names = ["a", "b", "c"];
        let mut f = Formula::default();
        let lits: Vec<Node> = names.iter().map(|n| f.push(*n, n.repeat(5).into_bytes())).collect();
        let enc = Encoder::new(Level::Zstd(3));
        let files = |a: &[u8]| -> Vec<(Vec<u8>, Vec<u8>)> {
            let Ok(Value::Tree(t, files)) = decode_value(a, &natives()) else { panic!("not an archive") };
            let mut got: Vec<_> = (t.entries.iter())
                .map(|e| match e.node {
                    EntryNode::File(c) => (e.name.clone(), files[c].clone()),
                    _ => unreachable!(),
                })
                .collect();
            got.sort();
            got
        };
        let file = |n: &str| (n.as_bytes().to_vec(), n.repeat(5).into_bytes());
        // a segment of file `k` committed to `w` and `out`, telling `status`
        let commit = |w: &mut SegmentWriter, out: &mut Vec<u8>, k: usize, status, base| {
            out.extend_from_slice(w.pad());
            let b = w.batch(&enc, &f, &[Leaf { outputs: vec![lits[k].clone()], sums: vec![None] }]).unwrap();
            w.commit(&b);
            out.extend_from_slice(&b.bytes);
            let contents = [ContentAt { leaf: b.leaf_at[0], output: 0, size: 5 }];
            let t = Tree { root: Extra::default(), entries: vec![entry(None, names[k], EntryNode::File(0))] };
            out.extend_from_slice(&w.close_as(&enc, &t, &contents, status).unwrap());
            let (end, old) = w.footer(base);
            out.extend_from_slice(&end);
            old
        };
        let left = Status::Incomplete { files: Some(2), bytes: Some(10) };
        let mut w = SegmentWriter::new();
        let mut out = SegmentWriter::header().to_vec();
        assert_eq!(commit(&mut w, &mut out, 0, left, 0), None);
        let first = out.clone();
        assert_eq!(locate(&first), Some(0..first.len()));
        assert_eq!(files(&first), [file("a")]);
        assert_eq!(open(&first, &natives()).unwrap().status(), Some(left));
        // the next commit written, not yet made: the archive before it, found either way
        let old = commit(&mut w, &mut out, 1, Status::Complete, 0).expect("the footer before");
        assert_eq!(locate(&out), Some(0..first.len()));
        assert_eq!(locate(&[&out, b"cut".as_slice()].concat()), Some(0..first.len()));
        assert_eq!(locate(&out[..out.len() - 3]), Some(0..first.len()));
        // made: both segments
        let second = killed(&out, old);
        assert_eq!(locate(&second), Some(0..second.len()));
        assert_eq!(files(&second), [file("a"), file("b")]);
        assert_eq!(open(&second, &natives()).unwrap().status(), Some(Status::Complete));
        // appended to by another writer, from what the archive tells of itself
        let footer =
            second.len() - 10 - u16::from_le_bytes([second[second.len() - 2], second[second.len() - 1]]) as usize;
        let mut at = footer + 8;
        let _ = leb(&second, &mut at).unwrap();
        let back = leb(&second, &mut at).unwrap() as usize;
        let mut w = SegmentWriter::appending(footer..second.len(), footer + 8 - back);
        let mut third = second.clone();
        let old = commit(&mut w, &mut third, 2, Status::Complete, 0).expect("the footer before");
        assert_eq!(old, footer..second.len());
        assert_eq!(locate(&third), Some(0..second.len()));
        let third = killed(&third, old);
        assert_eq!(files(&third), [file("a"), file("b"), file("c")]);
    }

    /// Journal points name states: a segment of no entries names the state again, and an archive
    /// opens as it is after any of them.
    #[test]
    fn journal_points() {
        let names = ["a", "b"];
        let mut f = Formula::default();
        let lits: Vec<Node> = names.iter().map(|n| f.push(*n, n.repeat(5).into_bytes())).collect();
        let enc = Encoder::new(Level::Zstd(3));
        let mut w = SegmentWriter::new();
        let mut out = SegmentWriter::header().to_vec();
        let mut olds = Vec::new();
        // (file `k` or nothing, the points)
        type Run<'a> = (Option<usize>, Vec<(u64, &'a str)>);
        let runs: [Run; 3] = [
            (Some(0), vec![(1000, "first")]),
            (Some(1), vec![(2000, "second"), (2500, "")]),
            (None, vec![(3000, "again")]),
        ];
        for (file, points) in runs {
            out.extend_from_slice(w.pad());
            let (t, contents) = match file {
                Some(k) => {
                    let b = w.batch(&enc, &f, &[Leaf { outputs: vec![lits[k].clone()], sums: vec![None] }]).unwrap();
                    w.commit(&b);
                    out.extend_from_slice(&b.bytes);
                    let t = Tree { root: Extra::default(), entries: vec![entry(None, names[k], EntryNode::File(0))] };
                    (t, vec![ContentAt { leaf: b.leaf_at[0], output: 0, size: 5 }])
                }
                None => (Tree::default(), Vec::new()),
            };
            w.set_journal(points.iter().map(|&(t, l)| (t, l.to_string())).collect()).unwrap();
            out.extend_from_slice(&w.close_as(&enc, &t, &contents, Status::Complete).unwrap());
            let (end, old) = w.footer(0);
            out.extend_from_slice(&end);
            olds.extend(old);
        }
        for old in olds.into_iter().rev() {
            out = killed(&out, old);
        }
        let want = [(1000, "first"), (2000, "second"), (2500, ""), (3000, "again")];
        let names_of = |a: &Archive| -> Vec<Vec<u8>> {
            let mut n: Vec<_> = a.tree().unwrap().entries.iter().map(|e| e.name.clone()).collect();
            n.sort();
            n
        };
        let nat = natives();
        let whole = open(&out, &nat).unwrap();
        let got: Vec<_> = whole.journal().iter().map(|p| (p.time, p.label.as_str())).collect();
        assert_eq!(got, want);
        assert_eq!(names_of(&whole), [b"a".to_vec(), b"b".to_vec()]);
        // the state after the points: the empty segment adds nothing
        let at: Vec<usize> = whole.journal().iter().map(|p| p.metadata).collect();
        assert!(at[0] < at[1] && at[1] == at[2] && at[2] < at[3]);
        let up = |m| open_upto(&out, &nat, m).unwrap();
        assert_eq!(names_of(&up(at[0])), [b"a".to_vec()]);
        assert_eq!(names_of(&up(at[1])), [b"a".to_vec(), b"b".to_vec()]);
        assert_eq!(up(at[0]).journal().len(), 1);
        assert_eq!(up(at[3]).journal().len(), 4);
        assert!(open_upto(&out, &nat, at[0] + 1).is_err());
        let indexed = |m| open_indexed_upto(Payload::Plain(&out), &nat, 1, m).unwrap();
        assert_eq!(names_of(&indexed(at[0])), [b"a".to_vec()]);
        assert_eq!(indexed(at[3]).journal().len(), 4);
        assert_eq!(open_indexed(Payload::Plain(&out), &nat, 1).unwrap().journal().len(), 4);
        // an archive's first segment has an entry; a time is not repeated
        let mut first = SegmentWriter::new();
        assert!(first.close_as(&enc, &Tree::default(), &[], Status::Complete).is_err());
        assert!(first.set_journal(vec![(1, "a".into()), (1, "b".into())]).is_err());
    }

    #[test]
    fn tombstones() {
        let mut f = Formula::default();
        let a = f.push("a", b"aaaaa".to_vec());
        let enc = Encoder::new(Level::Zstd(3));
        let mut w = SegmentWriter::new();
        let mut out = SegmentWriter::header().to_vec();
        let b = w.batch(&enc, &f, &[Leaf { outputs: vec![a.clone()], sums: vec![None] }]).unwrap();
        w.commit(&b);
        out.extend_from_slice(&b.bytes);
        let contents = [ContentAt { leaf: b.leaf_at[0], output: 0, size: 5 }];
        let dir = entry(None, "d", EntryNode::Dir);
        let t = Tree { root: Extra::default(), entries: vec![dir.clone(), entry(Some(0), "a", EntryNode::File(0))] };
        out.extend_from_slice(&w.close(&enc, &t, &contents).unwrap());
        // the next segment taking the directory away, and a file of nothing before
        let gone = |parent, name| entry(parent, name, EntryNode::TOMBSTONE);
        let t = Tree { root: Extra::default(), entries: vec![gone(None, "d"), gone(None, "x")] };
        out.extend_from_slice(&w.finish(&enc, &t, &[]).unwrap().0);
        let Ok(Value::Tree(t, _)) = decode_value(&out, &natives()) else { panic!() };
        assert!(t.entries.is_empty(), "{:?}", t.entries);
        // (no extra)
        let mut bad = gone(None, "d");
        bad.extra.mode = Some(0o755);
        assert!(w.finish(&enc, &Tree { root: Extra::default(), entries: vec![bad] }, &[]).is_err());
    }

    /// Paths looked up back from the last segment ([`super::super::open_indexed_at`]): as far as
    /// each is settled, the tree of the segments read having it as the whole archive's has it
    /// (and not entries of the others); a path under a directory taken away, or not there, not
    /// in it.
    #[test]
    fn looked_up() {
        use super::super::{Payload, open_indexed_at};
        let enc = Encoder::new(Level::Zstd(3));
        let mut w = SegmentWriter::new();
        let mut out = SegmentWriter::header().to_vec();
        // (of each segment: its entries, a file's by its data)
        let dir = |p: Option<usize>, n: &'static str| -> (Option<usize>, &str, Option<&str>) { (p, n, None) };
        let file = |p: Option<usize>, n: &'static str, d: &'static str| (p, n, Some(d));
        let segments = [
            vec![
                dir(None, "d"),
                file(Some(0), "a", "a0"),
                file(None, "b", "b0"),
                file(None, "c", "c0"),
                dir(None, "e"),
                file(Some(4), "x", "x0"),
            ],
            vec![dir(None, "d"), file(Some(0), "a", "a1"), file(None, "f", "f1")],
            vec![file(None, "g", "g2"), dir(None, "d"), file(Some(1), "n", "n2")],
        ];
        for (k, entries) in segments.iter().enumerate() {
            let mut f = Formula::default();
            let mut leaves = Vec::new();
            for (_, _, d) in entries {
                if let Some(d) = d {
                    let s = f.push("s", d.as_bytes().to_vec());
                    leaves.push(Leaf { outputs: vec![s], sums: vec![None] });
                }
            }
            let b = w.batch(&enc, &f, &leaves).unwrap();
            w.commit(&b);
            out.extend_from_slice(&b.bytes);
            let mut t = Tree::default();
            let mut contents = Vec::new();
            for &(parent, name, d) in entries {
                let node = match d {
                    Some(d) => {
                        contents.push(ContentAt { leaf: b.leaf_at[contents.len()], output: 0, size: d.len() as u64 });
                        EntryNode::File(contents.len() - 1)
                    }
                    None => EntryNode::Dir,
                };
                let mut e = entry(parent, name, node);
                e.extra.mode = Some(0o600 + 8 * k as u32 + t.entries.len() as u32);
                t.entries.push(e);
            }
            // (the middle one taking away "e" and what is under it)
            if k == 1 {
                t.entries.push(entry(None, "e", EntryNode::TOMBSTONE));
            }
            match k {
                2 => out.extend_from_slice(&w.finish(&enc, &t, &contents).unwrap().0),
                _ => out.extend_from_slice(&w.close(&enc, &t, &contents).unwrap()),
            }
        }
        let natives = natives();
        let names = |p: &str| -> Vec<Vec<u8>> { p.split('/').map(|n| n.as_bytes().to_vec()).collect() };
        // the entry at a path of a tree, each entry after its parent
        let find = |t: &Tree, p: &str| {
            let (mut at, mut names) = (None, names(p).into_iter());
            let mut name = names.next();
            for (i, e) in t.entries.iter().enumerate() {
                if name.as_ref().is_some_and(|n| e.parent == at && &e.name == n) {
                    (at, name) = (Some(i), names.next());
                }
            }
            if name.is_some() { None } else { at }
        };
        // the files under a path (or at it), by their paths, of an archive opened
        let files = |a: &super::super::Archive, p: &str| -> Vec<(String, Vec<u8>)> {
            let t = a.tree().unwrap();
            let Some(top) = find(t, p) else { return Vec::new() };
            let mut under = vec![false; t.entries.len()];
            let mut path = vec![String::new(); t.entries.len()];
            let mut only = vec![false; a.contents_len()];
            let mut of = std::collections::HashMap::new();
            for (i, e) in t.entries.iter().enumerate() {
                under[i] = i == top || e.parent.is_some_and(|q| under[q]);
                let name = String::from_utf8(e.name.clone()).unwrap();
                path[i] = e.parent.map_or(name.clone(), |q| format!("{}/{name}", path[q]));
                if let (true, EntryNode::File(c)) = (under[i], &e.node) {
                    only[*c] = true;
                    of.insert(*c, path[i].clone());
                }
            }
            let got = std::sync::Mutex::new(Vec::new());
            a.contents_of(2, Some(&only), |c, d| {
                got.lock().unwrap().push((of[&c].clone(), d));
                Ok(())
            })
            .unwrap();
            let mut got = got.into_inner().unwrap();
            got.sort();
            got
        };
        let whole = open_indexed(Payload::Plain(&out), &natives, 2).unwrap();
        let file = |p: &str, d: &str| (p.to_string(), d.as_bytes().to_vec());
        for (p, want, unread) in [
            // (the last segment alone: not "c" of the first)
            ("g", vec![file("g", "g2")], Some("c")),
            // (the last two: "a" a file of the second)
            ("d/a", vec![file("d/a", "a1")], Some("c")),
            ("c", vec![file("c", "c0")], None),
            ("d", vec![file("d/a", "a1"), file("d/n", "n2")], None),
            // (under "e", taken away by the second; not there at all)
            ("e/x", vec![], Some("c")),
            ("y", vec![], None),
        ] {
            let a = open_indexed_at(Payload::Plain(&out), &natives, 2, &[names(p)]).unwrap();
            assert_eq!(files(&a, p), want, "{p}");
            assert_eq!(files(&whole, p), want, "{p}");
            // (the extras of those at the path and under it as they are; "f" of the segment
            // read, not under it, not read)
            let (t, all) = (a.tree().unwrap(), whole.tree().unwrap());
            if let Some(top) = find(t, p) {
                let mut under = vec![false; t.entries.len()];
                for (i, e) in t.entries.iter().enumerate() {
                    under[i] = i == top || e.parent.is_some_and(|q| under[q]);
                }
                for i in (0..t.entries.len()).filter(|&i| under[i]) {
                    let w = all.entries.iter().find(|e| e.name == t.entries[i].name && e.node != EntryNode::Dir);
                    assert!(t.entries[i].extra.mode.is_some(), "{p}: #{i}");
                    if let Some(w) = w.filter(|_| t.entries[i].node != EntryNode::Dir) {
                        assert_eq!(t.entries[i].extra.mode, w.extra.mode, "{p}: #{i}");
                    }
                }
            }
            if p == "d/a" {
                let f = find(t, "f").expect("of the segment read");
                assert_eq!(t.entries[f].extra.mode, None);
                assert!(all.entries[find(all, "f").unwrap()].extra.mode.is_some());
            }
            if let Some(u) = unread {
                assert!(find(a.tree().unwrap(), u).is_none(), "{p}: {u} read");
            }
            // (all of it not read so)
            assert!(unread.is_none() || a.contents(1, |_, _| Ok(())).is_err());
        }
    }

    #[test]
    fn footers_off_sector_boundaries() {
        let mut f = Formula::default();
        let s = f.push("a", b"aaaaa".to_vec());
        let enc = Encoder::new(Level::Zstd(3));
        // the archive put at every offset of a sector in a file: its footers never across one
        for base in 0..512u64 {
            let mut w = SegmentWriter::new();
            let mut out = SegmentWriter::header().to_vec();
            let b = w.batch(&enc, &f, &[Leaf { outputs: vec![s.clone()], sums: vec![None] }]).unwrap();
            w.commit(&b);
            out.extend_from_slice(&b.bytes);
            let contents = [ContentAt { leaf: b.leaf_at[0], output: 0, size: 5 }];
            let t = Tree { root: Extra::default(), entries: vec![entry(None, "a", EntryNode::File(0))] };
            let (ended, _) = w.end(&enc, &t, &contents, Status::Complete, base).unwrap();
            let meta = w.close_as(&enc, &t, &contents, Status::Complete).unwrap();
            let (end, _) = w.footer(base);
            // (the same, ended without closing)
            assert_eq!(ended, [&meta[..], &end].concat());
            out.extend_from_slice(&meta);
            out.extend_from_slice(&end);
            let n = u16::from_le_bytes([out[out.len() - 2], out[out.len() - 1]]) as usize;
            let at = out.len() - n - 10;
            assert!(!crossing(base + at as u64, kill(n + 10).len()), "{base}");
            assert!(decode_value(&out, &natives()).is_ok(), "{base}");
        }
        assert_eq!(kill(21), [0, 20]);
        assert_eq!(kill(300), [1, 0x29, 1]);
        assert!(crossing(511, 2) && !crossing(510, 2) && !crossing(512, 2) && crossing(1022, 3));
    }

    /// An archive of three segments, each closed by its metadata block: each writes the module
    /// it applies again and takes nothing of the others'; the trees merge (spec, "Merging
    /// segments"): a directory again (its extra kept where it has none, replaced where it has
    /// one), a file replaced, a directory replaced by a file, and link groups of a segment's own
    /// apart from another's of the same number.
    #[test]
    fn several_segments() {
        let names = ["d/a", "x", "h1", "h2", "d/b", "x2", "h3", "d"];
        let mut f = Formula::default();
        let lits: Vec<Node> = names.iter().map(|n| f.push(*n, n.as_bytes().to_vec())).collect();
        let enc = Encoder::new(Level::Zstd(3));
        let mut w = SegmentWriter::new();
        let mut out = SegmentWriter::header().to_vec();
        // each segment's leaves (by name) and tree
        let file = |parent, name: &str, c, group: Option<i128>| Entry {
            extra: Extra { link_group: group, ..Extra::default() },
            ..entry(parent, name, EntryNode::File(c))
        };
        let dir =
            |mode: Option<u32>| Entry { extra: Extra { mode, ..Extra::default() }, ..entry(None, "d", EntryNode::Dir) };
        let segments: [(Vec<usize>, Vec<Entry>); 3] = [
            (
                vec![0, 1, 2],
                vec![
                    dir(Some(0o700)),
                    file(Some(0), "a", 0, None),
                    file(None, "x", 1, None),
                    file(None, "h1", 2, Some(0)),
                    file(None, "h2", 2, Some(0)),
                ],
            ),
            (
                vec![4, 5, 6],
                vec![dir(None), file(Some(0), "b", 0, None), file(None, "x", 1, None), file(None, "h3", 2, Some(0))],
            ),
            (vec![7], vec![file(None, "d", 0, Some(-5))]),
        ];
        let modules = |b: &Batch| b.layout.iter().filter(|p| p.0 == Placed::Module).count();
        for (k, (of, entries)) in segments.iter().enumerate() {
            let leaves: Vec<Leaf> = of
                .iter()
                .map(|&i| Leaf {
                    outputs: vec![Node::apply(REV, vec![1, 2, 3], vec![lits[i].clone()], names[i].len() as u64)],
                    sums: vec![None],
                })
                .collect();
            let b = w.batch(&enc, &f, &leaves).unwrap();
            assert_eq!(modules(&b), 1);
            w.commit(&b);
            out.extend_from_slice(&b.bytes);
            let contents: Vec<ContentAt> = (of.iter().enumerate())
                .map(|(j, &i)| ContentAt { leaf: b.leaf_at[j], output: 0, size: names[i].len() as u64 })
                .collect();
            let t = Tree { root: Extra::default(), entries: entries.clone() };
            if k < 2 {
                out.extend_from_slice(&w.close(&enc, &t, &contents).unwrap());
                assert!(w.segment_is_empty());
                // (a content of the segment closed is not of the next)
                assert!(w.close(&enc, &t, &contents).is_err());
            } else {
                out.extend_from_slice(&w.finish(&enc, &t, &contents).unwrap().0);
            }
        }
        let Ok(Value::Tree(t, files)) = decode_value(&out, &natives()) else { panic!() };
        let rev = |s: &str| s.bytes().rev().collect::<Vec<u8>>();
        let got: Vec<(&[u8], Vec<u8>, Option<i128>)> = (t.entries.iter())
            .map(|e| match e.node {
                EntryNode::File(c) => (&e.name[..], files[c].clone(), e.extra.link_group),
                _ => (&e.name[..], Vec::new(), None),
            })
            .collect();
        assert_eq!(
            got,
            [
                (&b"h1"[..], rev("h1"), Some(0)),
                (b"h2", rev("h1"), Some(0)),
                (b"x", rev("x2"), None),
                (b"h3", rev("h3"), Some(1)),
                (b"d", rev("d"), Some(-5)),
            ]
        );
        assert!(t.entries.iter().all(|e| e.parent.is_none()));
        // (only the contents left in the tree)
        assert_eq!(files.len(), 4);

        // the second segment's directory over the first's: of the first's extra, or of its own
        let mut both = Vec::new();
        for mode in [None, Some(0o755)] {
            let mut w = SegmentWriter::new();
            let mut out = SegmentWriter::header().to_vec();
            for (k, (of, entries)) in segments[..2].iter().enumerate() {
                let leaves: Vec<Leaf> =
                    of.iter().map(|&i| Leaf { outputs: vec![lits[i].clone()], sums: vec![None] }).collect();
                let b = w.batch(&enc, &f, &leaves).unwrap();
                w.commit(&b);
                out.extend_from_slice(&b.bytes);
                let contents: Vec<ContentAt> = (of.iter().enumerate())
                    .map(|(j, &i)| ContentAt { leaf: b.leaf_at[j], output: 0, size: names[i].len() as u64 })
                    .collect();
                let mut t = Tree { root: Extra::default(), entries: entries.clone() };
                if k == 0 {
                    out.extend_from_slice(&w.close(&enc, &t, &contents).unwrap());
                } else {
                    t.entries[0].extra.mode = mode;
                    out.extend_from_slice(&w.finish(&enc, &t, &contents).unwrap().0);
                }
            }
            let Ok(Value::Tree(t, files)) = decode_value(&out, &natives()) else { panic!() };
            let d = t.entries.iter().position(|e| e.name == b"d").unwrap();
            let mut under: Vec<Vec<u8>> = (t.entries.iter())
                .filter(|e| e.parent == Some(d))
                .map(|e| match e.node {
                    EntryNode::File(c) => files[c].clone(),
                    _ => unreachable!(),
                })
                .collect();
            under.sort();
            assert_eq!(under, [b"d/a".to_vec(), b"d/b".to_vec()]);
            both.push(t.entries[d].extra.mode);
        }
        assert_eq!(both, [Some(0o700), Some(0o755)]);
    }
}
