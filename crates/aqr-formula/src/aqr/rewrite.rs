//! An archive written again of what its tree, edited, still holds ([`rewrite`]): `aqr
//! --compact`. Segment by segment, as they are (a segment of none of the tree's contents left
//! out), each block it still needs copied as it is: its module, resource and dictionary blocks
//! (all of them, as the formula blocks refer to them by their indices in the segment), the leaf
//! blocks of the tree's contents, and the non-leaf blocks those refer to. Spacer blocks, the size
//! hint block and the blocks of no meaning known are left out.
//!
//! A non-leaf block that a file taken away sliced too is made again of the bytes still sliced
//! (the ranges of the Slice instructions of the formula blocks kept, merged), compressed again
//! without a dictionary: what was solid with the files taken away leaves the file too.
//!
//! A formula block refers to non-leaf blocks by their ordinals among those after it, and slices
//! them by offsets (spec, "Formula data"): where blocks between it and those it refers to are
//! left out, its ordinals are made again, and where those it slices are made again, the starts
//! of its slices; its formula data is then compressed again (without a dictionary). Its
//! instructions are otherwise kept as they are, its applications too.
//!
//! Each segment's metadata block is made again: the entries whose contents are of it, at their
//! paths in the tree, with the directories on the way (of no extra, which keeps theirs); the
//! entries of no content (directories, links, ...) in the last. No tombstone is left. The
//! metadata this crate does not read ([`Extra`]) would be lost, so an archive having any is not
//! written again.

use std::collections::HashMap;
use std::io::Write;

use aqr_common::varint;

use super::segment::Copied;
use super::tree::{Entry, EntryNode, Extra, SegmentInfo, Status, Tree};
use super::{
    ContentAt, Encoder, Failure, HEADER_MAGIC, LeafBlock, Level, Reader, SegmentWriter, always_compressed, critical,
    decoded_with, footer_of, formula_block, frame, inspect, opened, opened_with,
};
use crate::{Error, Result};

/// What [`rewrite`] wrote.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Rewritten {
    /// The bytes of the archive.
    pub len: u64,
    /// The segments, of the archive written and of the one before.
    pub segments: (usize, usize),
    /// The leaf and non-leaf blocks left out, the non-leaf blocks made again of the bytes still
    /// sliced, and the formula blocks made again.
    pub leaves_left: usize,
    pub non_leaf_left: usize,
    pub non_leaf_packed: usize,
    pub formulas_made: usize,
    /// The leaf blocks made again (formula blocks), by their indices among those written: to be
    /// evaluated to check them.
    pub made: Vec<usize>,
}

/// What becomes of the journal points of an archive written again ([`rewrite_as`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum History {
    /// Fails where the archive has any: they would be lost.
    Refuse,
    /// Every point dropped, and with them the states between: the archive is of its tree alone.
    Drop,
    /// The points from the time `since` on kept, with their states: the segments up to the one
    /// whose metadata block is at `from` (the first point's) written again as one state, those
    /// after it as they are.
    Keep { from: usize, since: u64 },
}

/// What a block of the archive is, as it is written again.
#[derive(Debug, Clone, Copy)]
enum Kind {
    Aux(Copied),
    /// A leaf block (by its index among all), and a non-leaf block (by its ordinal).
    Leaf(usize),
    NonLeaf(usize),
    /// Left out: a spacer block, the size hint block, a block of no meaning known.
    Dropped,
}

/// Archive `c` (an unencrypted standalone archive of a tree, as it is located) written again
/// to `out` of tree `t`: the archive's tree ([`super::Archive::tree`]), edited (entries taken
/// away, moved, their extras changed), its contents those of the archive (module).
/// Fails on an archive of mirror blocks, or of metadata this crate does not read, and on a tree
/// of no entries.
pub fn rewrite(c: &[u8], t: &Tree, level: Level, threads: usize, out: &mut dyn Write) -> Result<Rewritten> {
    rewrite_as(c, t, level, threads, out, History::Refuse)
}

/// [`rewrite`], `history` telling what becomes of the journal points. With [`History::Keep`], `t`
/// is not used: the archive is of its own tree, the segments after the first point kept written
/// as they are (their entries, tombstones, `segment-status` and `journal`, their blocks all
/// copied), so that the states at the points still read as they did.
pub fn rewrite_as(
    c: &[u8],
    t: &Tree,
    level: Level,
    threads: usize,
    out: &mut dyn Write,
    history: History,
) -> Result<Rewritten> {
    let fail = |f: Failure| Error::new(f.to_string());
    if !c.starts_with(&HEADER_MAGIC) {
        return Err(Error::new("not a standalone archive"));
    }
    unknown_metadata(c, history != History::Refuse)?;
    let keeping = match history {
        History::Keep { from, since } => Some((from, since)),
        _ => None,
    };
    let (blocks, tree, ..) = opened(c, false, false, keeping.map(|k| k.0)).map_err(fail)?;
    let (own, refs) = tree.ok_or_else(|| Error::new("an archive of a single file, not of paths"))?;
    // (the segments before the first point kept, as one state; and each segment as it is told)
    let mut infos: Vec<SegmentInfo> = Vec::new();
    let base_n = match keeping {
        Some((from, _)) => {
            opened_with(c, false, false, None, Some(&mut infos)).map_err(fail)?;
            let at = |s: &super::Segmented| s.metadata.as_ref().map(|m| m.0);
            let k = blocks.segments.iter().position(|s| at(s) == Some(from));
            k.ok_or_else(|| Error::new("no metadata block where the first point is"))? + 1
        }
        None => blocks.segments.len(),
    };
    let t = if keeping.is_some() { &own } else { t };
    if t.entries.is_empty() {
        return Err(Error::new("nothing left in it"));
    }
    let (footer, _) = footer_of(c).ok_or_else(|| Error::new("no footer"))?;

    // every block, by segment, as it is written again
    let mut segments: Vec<Vec<(usize, usize, Kind)>> = vec![Vec::new()];
    let (mut leaf, mut non_leaf) = (0, 0);
    for (start, ty, _, size) in frame(&c[HEADER_MAGIC.len()..footer.start]).map_err(fail)? {
        let at = HEADER_MAGIC.len() + start;
        let kind = match ty {
            0x10..=0x17 => {
                segments.push(Vec::new());
                continue;
            }
            0x20..=0x37 | 0x70..=0x77 => {
                leaf += 1;
                Kind::Leaf(leaf - 1)
            }
            0x40..=0x4b => {
                non_leaf += 1;
                Kind::NonLeaf(non_leaf - 1)
            }
            0x51..=0x57 => Kind::Aux(Copied::Dictionary),
            0x61..=0x63 => Kind::Aux(Copied::Module),
            0x64..=0x67 => Kind::Aux(Copied::Resource),
            _ if super::mirror::is_mirror(ty) => {
                return Err(Error::new("an archive of mirror blocks (-A) is not written again yet"));
            }
            0x00..=0x04 => Kind::Dropped,
            _ if !critical(ty) => Kind::Dropped,
            _ => return Err(Error::new(format!("a block of type {ty:#x} not written again"))),
        };
        segments.last_mut().unwrap().push((at, size as usize, kind));
    }
    // (none after the last metadata block, as the archive opened)
    segments.pop();
    debug_assert_eq!(segments.len(), blocks.segments.len());

    // the leaves the tree's contents are of, and the segment of each
    let mut live = vec![false; blocks.leaves.len()];
    let contents_of = |x: &Extra, node: Option<&EntryNode>| {
        let file = match node {
            Some(&EntryNode::File(k)) => Some(k),
            _ => None,
        };
        file.into_iter().chain(x.forks.iter().map(|f| f.1)).collect::<Vec<_>>()
    };
    let all = std::iter::once(contents_of(&t.root, None))
        .chain(t.entries.iter().map(|e| contents_of(&e.extra, Some(&e.node))))
        .collect::<Vec<_>>();
    for &k in all.iter().flatten() {
        let r = refs.get(k).ok_or_else(|| Error::new(format!("no content #{k} in the archive")))?;
        live[r.0] = true;
    }
    // (the segments after the first point kept are copied whole)
    for seg in &blocks.segments[base_n..] {
        live[seg.first.leaves..seg.end.leaves].fill(true);
    }
    let segment_of = |leaf: usize| blocks.segments.iter().position(|s| (s.first.leaves..s.end.leaves).contains(&leaf));
    let kept: Vec<usize> = (0..base_n)
        .filter(|&s| {
            let seg = &blocks.segments[s];
            live[seg.first.leaves..seg.end.leaves].iter().any(|&l| l)
        })
        .collect();
    let new_segment: HashMap<usize, usize> = kept.iter().enumerate().map(|(n, &s)| (s, n)).collect();
    let last = kept.len().saturating_sub(1);
    // (the segment each entry, and the root, goes in: that of its contents, else the last)
    let home = |cs: &[usize]| -> Result<usize> {
        let mut segs = cs.iter().map(|&k| new_segment[&segment_of(refs[k].0).expect("a leaf of a segment")]);
        let first = segs.next().unwrap_or(last);
        match segs.all(|s| s == first) {
            true => Ok(first),
            false => Err(Error::new("an entry of contents of several segments")),
        }
    };
    let homes = all.iter().map(|cs| home(cs)).collect::<Result<Vec<usize>>>()?;

    let enc = Encoder::with_threads(level, threads);
    // (the segments written: those of the state, then those after it as they are)
    let after = blocks.segments.len() - base_n;
    let (base, total) = (kept.len().max(1), kept.len().max(1) + after);
    let mut r = Rewritten { segments: (total, blocks.segments.len()), ..Rewritten::default() };
    let mut w = SegmentWriter::new();
    let io = |e: std::io::Error| Error::new(e.to_string());
    out.write_all(w.head()).map_err(io)?;
    let mut written = w.len() as u64;
    let mut leaf_at = vec![usize::MAX; blocks.leaves.len()];
    let mut leaves_written = 0;
    for n in 0..total {
        let s = if n < base { kept.get(n).copied() } else { Some(base_n + n - base) };
        if let Some(s) = s {
            let seg = &blocks.segments[s];
            let leaves = seg.first.leaves..seg.end.leaves;
            // the non-leaf blocks still referred to, the bytes of each still sliced, and the
            // ordinals of each formula block's
            let whole = live[leaves.clone()].iter().all(|&l| l);
            let nl = seg.first.non_leaf;
            let mut keep = vec![whole; seg.end.non_leaf - nl];
            let mut programs: HashMap<usize, Referred> = HashMap::new();
            let mut packed: HashMap<usize, Packed> = HashMap::new();
            if !whole {
                let mut dead = vec![false; keep.len()];
                let mut sliced: Vec<Vec<(u64, u64)>> = vec![Vec::new(); keep.len()];
                for l in leaves.clone() {
                    if let Some(p) = referred(&blocks, l).map_err(fail)? {
                        for &o in &p.ordinals {
                            *if live[l] { &mut keep[o - nl] } else { &mut dead[o - nl] } = true;
                        }
                        if live[l] {
                            p.slices
                                .iter()
                                .for_each(|x| sliced[p.ordinals[x.block] - nl].push((x.start, x.start + x.len)));
                            programs.insert(l, p);
                        }
                    }
                }
                // (those a file taken away sliced too: of the bytes still sliced alone)
                for i in (0..keep.len()).filter(|&i| keep[i] && dead[i]) {
                    if let Some(p) = pack(&blocks, nl + i, &mut sliced[i], &enc)? {
                        packed.insert(nl + i, p);
                    }
                }
            }
            // (of a non-leaf ordinal, those kept before it in the segment)
            let mut rank = Vec::with_capacity(keep.len() + 1);
            rank.push(0);
            for &k in &keep {
                rank.push(rank.last().unwrap() + k as usize);
            }
            let rank_of = |o: usize| rank[o - nl];
            for &(at, size, kind) in &segments[s] {
                let bytes = &c[at..at + size];
                let (what, made) = match kind {
                    Kind::Aux(a) => (a, None),
                    Kind::Leaf(l) if !live[l] => {
                        r.leaves_left += 1;
                        continue;
                    }
                    Kind::Leaf(l) => {
                        let dictionary = blocks.leaf_dictionary[l].map(|d| d - seg.first.dictionaries);
                        let again = match (programs.get(&l), &blocks.leaves[l]) {
                            (Some(p), &LeafBlock::Formula { outputs, non_leaf, .. }) => {
                                again(p, outputs, rank_of(non_leaf), &rank_of, &packed, &enc)?
                            }
                            _ => None,
                        };
                        leaf_at[l] = w.len();
                        leaves_written += 1;
                        match again {
                            Some(b) => {
                                r.formulas_made += 1;
                                r.made.push(leaves_written - 1);
                                (Copied::Leaf(None), Some(b))
                            }
                            None => (Copied::Leaf(dictionary), None),
                        }
                    }
                    Kind::NonLeaf(o) if !keep[o - seg.first.non_leaf] => {
                        r.non_leaf_left += 1;
                        continue;
                    }
                    Kind::NonLeaf(o) => match packed.remove(&o) {
                        Some(p) => {
                            r.non_leaf_packed += 1;
                            (Copied::NonLeaf, Some(p.block))
                        }
                        None => (Copied::NonLeaf, None),
                    },
                    Kind::Dropped => continue,
                };
                let bytes = made.as_deref().unwrap_or(bytes);
                out.write_all(bytes).map_err(io)?;
                w.copied(what, bytes.len());
                written += bytes.len() as u64;
            }
        }
        // the entries of the segment
        let (st, contents) = match n < base {
            true => segment_tree(t, &homes, n, &|k| {
                let (l, output, size) = refs[k];
                ContentAt { leaf: leaf_at[l], output, size }
            }),
            false => told_tree(&infos[base_n + n - base], &leaf_at)?,
        };
        if n < base && st.entries.is_empty() {
            return Err(Error::new("a segment of no entries (of the root's forks alone)"));
        }
        // (the writing's end and the points: of the segment ended at, for the one the state is
        // of; as they were, after it)
        let (status, journal) = match keeping {
            None => (if n + 1 == total { Status::Complete } else { Status::None }, Vec::new()),
            Some((_, since)) if n + 1 == base => {
                let first = &infos[base_n - 1];
                (first.status, first.journal.iter().filter(|p| p.0 >= since).cloned().collect())
            }
            Some(_) if n < base => (Status::None, Vec::new()),
            Some(_) => {
                let told = &infos[base_n + n - base];
                (told.status, told.journal.clone())
            }
        };
        w.set_journal(journal)?;
        let end = match n + 1 == total {
            false => w.close_as(&enc, &st, &contents, status)?,
            true => w.finish_as(&enc, &st, &contents, status)?.0,
        };
        out.write_all(&end).map_err(io)?;
        written += end.len() as u64;
    }
    r.len = written;
    Ok(r)
}

/// Fails where archive `c` has metadata that [`Extra`] does not hold (a comment, ZIP's extra
/// fields, members of no meaning known), which writing it again would lose.
fn unknown_metadata(c: &[u8], journal: bool) -> Result<()> {
    let i = inspect::inspect(c, false, &|_| None, None).map_err(|f| Error::new(f.to_string()))?;
    let known = |name: Option<&str>| name.is_some_and(|n| !matches!(n, "comment" | "zip-extras"));
    for s in &i.segments {
        let named = |m: &super::tree::Member| m.key.and_then(super::archive_key_name);
        if !journal && s.archive_extra.iter().any(|m| named(m) == Some("journal")) {
            return Err(Error::new("the journal's points would be lost in writing it again"));
        }
        if let Some(m) = s.archive_extra.iter().find(|m| !matches!(named(m), Some("segment-status" | "journal"))) {
            return Err(Error::new(format!("archive-wide metadata not kept in writing it again: {}", m.key_text)));
        }
        for e in &s.entries {
            let members = e.members.iter().flatten();
            if let Some(m) = members.into_iter().find(|m| !known(m.key.and_then(super::entry_key_name))) {
                return Err(Error::new(format!("metadata not kept in writing it again: {}", m.key_text)));
            }
        }
    }
    Ok(())
}

/// The tree of a segment as it was told (`told`), its contents at their leaves in the archive
/// written (`leaf_at`, by the leaf's index in the one read): the tombstones among its entries,
/// after the others. Fails where an entry extra is told as a map of no member, which the entry
/// would lose (null) in it.
fn told_tree(told: &SegmentInfo, leaf_at: &[usize]) -> Result<(Tree, Vec<ContentAt>)> {
    let st = &told.tree;
    let empty = |k: usize| st.given.get(k) == Some(&true) && st.tree.entries[k - 1].extra == Extra::default();
    if (1..=st.tree.entries.len()).any(empty) || (st.given.first() == Some(&true) && st.tree.root == Extra::default()) {
        return Err(Error::new("an entry extra of no member (an empty map) is not kept in writing it again"));
    }
    let mut tree = st.tree.clone();
    for (parent, name) in &st.tombstones {
        tree.entries.push(Entry {
            parent: *parent,
            name: name.clone(),
            node: EntryNode::Special(0),
            extra: Extra::default(),
        });
    }
    let contents = (st.refs.iter()).map(|&(l, output, size)| ContentAt { leaf: leaf_at[l], output, size }).collect();
    Ok((tree, contents))
}

/// A formula block's references to non-leaf blocks ([`referred`]): their ordinals among all,
/// its formula data decoded, where the list of them ends in it, and its Slice instructions.
struct Referred {
    ordinals: Vec<usize>,
    data: Vec<u8>,
    prefix: usize,
    slices: Vec<SliceAt>,
}

/// A Slice instruction: of the referenced item `block` (by its index in the formula's list), and
/// where its start operand is in the formula data.
struct SliceAt {
    block: usize,
    start: u64,
    len: u64,
    at: std::ops::Range<usize>,
}

/// A non-leaf block made again of the bytes still sliced ([`pack`]): its bytes, and where each
/// range kept (start, end) starts in it.
struct Packed {
    block: Vec<u8>,
    ranges: Vec<(u64, u64, u64)>,
}

impl Packed {
    /// Where a slice of `len` bytes at `start` of the block before is in this one.
    fn start(&self, start: u64, len: u64) -> u64 {
        let i = self.ranges.partition_point(|r| r.1 < start + len.max(1)).min(self.ranges.len().saturating_sub(1));
        match self.ranges.get(i) {
            Some(&(s, e, at)) if s <= start && start + len <= e => at + (start - s),
            // (an empty slice where no range is)
            _ => 0,
        }
    }
}

/// What formula block `l` refers to of the non-leaf blocks; none of a leaf data block.
fn referred(blocks: &super::Blocks, l: usize) -> super::Decoded<Option<Referred>> {
    let LeafBlock::Formula { data, non_leaf, .. } = blocks.leaves[l] else { return Ok(None) };
    let (d, dict) = blocks.formulas[data];
    let d = decoded_with(d, true, true, dict.map(|i| &blocks.dictionaries[i]))?;
    let mut r = Reader { d: &d, at: 0 };
    let mut next = non_leaf;
    let mut ordinals = Vec::new();
    for delta in r.nums()? {
        let o = next + delta as usize;
        ordinals.push(o);
        next = o + 1;
    }
    let prefix = r.at;
    let slices = slices(&d, prefix, ordinals.len())?;
    Ok(Some(Referred { ordinals, data: d, prefix, slices }))
}

/// The Slice instructions of formula data `d`, its resources' and modules' lists from `at` on,
/// of `blocks` referenced items: the instructions' operands read (spec, "Formula data"), not
/// checked further than they have to be to be read.
fn slices(d: &[u8], at: usize, blocks: usize) -> super::Decoded<Vec<SliceAt>> {
    use super::{
        APPLY, APPLY_OLD, CHECKSUM, CONCAT, LITERAL, MODULE_BYTES, NOP, PACK, PATCH, RESOURCE_BYTES, SLICE, SPLIT, SWAP,
    };
    let mut r = Reader { d, at };
    r.nums()?;
    r.nums()?;
    let mut out = Vec::new();
    while r.at < d.len() {
        let op = d[r.at];
        r.at += 1;
        match op {
            NOP => {}
            LITERAL => _ = r.bytes()?,
            MODULE_BYTES | RESOURCE_BYTES => _ = (r.num()?, r.num()?),
            SWAP | CONCAT | PACK | PATCH => _ = r.num()?,
            SLICE => {
                let block = r.index(blocks)?;
                let s = r.at;
                let start = r.num()?;
                let at = s..r.at;
                out.push(SliceAt { block, start, len: r.num()?, at });
            }
            APPLY | APPLY_OLD => {
                r.num()?;
                r.nums()?;
                r.bytes()?;
                r.num()?;
                if op == APPLY {
                    r.nums()?;
                }
                r.num()?;
                r.nums()?;
            }
            SPLIT => _ = r.nums()?,
            CHECKSUM if r.at + 8 <= d.len() => r.at += 8,
            _ => return Err(Failure::Invalid(format!("formula data not read at opcode {op:#04x}"))),
        }
    }
    Ok(out)
}

/// Non-leaf block `o` made again of its bytes in `sliced` (the ranges sliced of it, sorted and
/// merged here), compressed at the level of `enc` without a dictionary: none where they are all
/// of it.
fn pack(blocks: &super::Blocks, o: usize, sliced: &mut Vec<(u64, u64)>, enc: &Encoder) -> Result<Option<Packed>> {
    let (data, zstd, dict) = blocks.non_leaf[o];
    let d =
        decoded_with(data, zstd, true, dict.map(|i| &blocks.dictionaries[i])).map_err(|f| Error::new(f.to_string()))?;
    sliced.retain(|r| r.0 < r.1);
    sliced.sort_unstable();
    let mut merged: Vec<(u64, u64)> = Vec::new();
    for &(s, e) in sliced.iter() {
        match merged.last_mut() {
            Some(m) if s <= m.1 => m.1 = m.1.max(e),
            _ => merged.push((s, e)),
        }
    }
    if merged == [(0, d.len() as u64)] {
        return Ok(None);
    }
    let mut kept = Vec::new();
    let mut ranges = Vec::with_capacity(merged.len());
    for (s, e) in merged {
        ranges.push((s, e, kept.len() as u64));
        kept.extend_from_slice(&d[s as usize..e as usize]);
    }
    let mut block = Vec::new();
    enc.content_block(&mut block, false, &[&kept], super::How::Usual, None)?;
    Ok(Some(Packed { block, ranges }))
}

/// Formula block of `outputs` outputs referring as `p` does, made again where the non-leaf
/// blocks kept before it are `first` and before each, `rank`, and where those it slices were
/// `packed`: none where it refers to them the same.
fn again(
    p: &Referred,
    outputs: u64,
    first: usize,
    rank: &dyn Fn(usize) -> usize,
    packed: &HashMap<usize, Packed>,
    enc: &Encoder,
) -> Result<Option<Vec<u8>>> {
    let d = &p.data;
    let mut deltas = Vec::with_capacity(p.ordinals.len());
    let mut next = first;
    for &o in &p.ordinals {
        let n = rank(o);
        deltas.push((n - next) as u64);
        next = n + 1;
    }
    // (none left out between it and those it refers to, nor of them: as it is)
    let before = Reader { d, at: 0 }.nums().map_err(|f| Error::new(f.to_string()))?;
    let moved: Vec<(&SliceAt, u64)> = (p.slices.iter())
        .filter_map(|x| packed.get(&p.ordinals[x.block]).map(|k| (x, k.start(x.start, x.len))))
        .collect();
    if before == deltas && moved.is_empty() {
        return Ok(None);
    }
    let mut data = Vec::with_capacity(d.len());
    varint::put(&mut data, deltas.len() as u64);
    deltas.iter().for_each(|&x| varint::put(&mut data, x));
    let mut from = p.prefix;
    for (x, start) in moved {
        data.extend_from_slice(&d[from..x.at.start]);
        varint::put(&mut data, start);
        from = x.at.end;
    }
    data.extend_from_slice(&d[from..]);
    let mut out = Vec::new();
    formula_block(&mut out, outputs, &always_compressed(&mut enc.cctx(), &[&data], enc.base_level(), enc.threads)?);
    Ok(Some(out))
}

/// Of tree `t` (its entries in segment `homes[k + 1]`, the root's extra in `homes[0]`), the
/// tree of segment `n`: its own entries, with the directories on the way (of no extra), and
/// their contents (each where `at` puts it).
fn segment_tree(t: &Tree, homes: &[usize], n: usize, at: &dyn Fn(usize) -> ContentAt) -> (Tree, Vec<ContentAt>) {
    let own = |k: usize| homes[k + 1] == n;
    let len = t.entries.len();
    let mut taken: Vec<bool> = (0..len).map(own).collect();
    for k in (0..len).rev() {
        if taken[k]
            && let Some(p) = t.entries[k].parent
        {
            taken[p] = true;
        }
    }
    let mut contents: Vec<ContentAt> = Vec::new();
    let mut number: HashMap<usize, usize> = HashMap::new();
    let mut content = |k: usize, contents: &mut Vec<ContentAt>| {
        *number.entry(k).or_insert_with(|| {
            contents.push(at(k));
            contents.len() - 1
        })
    };
    let mut index = vec![None; len];
    let mut entries = Vec::new();
    for k in (0..len).filter(|&k| taken[k]) {
        let e = &t.entries[k];
        let mut extra = if own(k) { e.extra.clone() } else { Extra::default() };
        extra.forks.iter_mut().for_each(|f| f.1 = content(f.1, &mut contents));
        let node = match e.node {
            EntryNode::File(c) => EntryNode::File(content(c, &mut contents)),
            ref other => other.clone(),
        };
        index[k] = Some(entries.len());
        let parent = e.parent.map(|p| index[p].expect("a directory on the way taken"));
        entries.push(Entry { parent, name: e.name.clone(), node, extra });
    }
    let mut root = if homes[0] == n { t.root.clone() } else { Extra::default() };
    root.forks.iter_mut().for_each(|f| f.1 = content(f.1, &mut contents));
    (Tree { root, entries }, contents)
}

#[cfg(test)]
mod tests {
    use super::super::tests::natives;
    use super::super::{Leaf, Segment, Value, checksum, decode_value, open};
    use super::*;
    use crate::{Formula, Node};

    fn text(seed: u64, n: usize) -> Vec<u8> {
        let mut x = seed;
        (0..n)
            .map(|_| {
                x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                b"abcdefgh "[(x >> 60) as usize % 9]
            })
            .collect()
    }

    /// An archive of a group of leaves over two streams (the first sliced by the first leaf
    /// alone), and a leaf data block; its files' contents.
    fn grouped() -> (Vec<u8>, Vec<Vec<u8>>) {
        let mut f = Formula::default();
        let (a, b) = (text(1, 30_000), text(2, 30_000));
        f.push("a", a.clone());
        f.push("b", b.clone());
        let own = f.push("own", text(3, 5000));
        let outs =
            [Node::Concat(vec![Node::slice(0, 0, 1000), Node::slice(1, 0, 1000)]), Node::slice(1, 500, 2000), own];
        let want = vec![[&a[..1000], &b[..1000]].concat(), b[500..2500].to_vec(), text(3, 5000)];
        let leaves: Vec<Leaf> = outs
            .into_iter()
            .zip(&want)
            .map(|(o, w)| Leaf { outputs: vec![o], sums: vec![Some(checksum(w))] })
            .collect();
        let mut tree = Tree::default();
        tree.entries.push(Entry { parent: None, name: b"d".to_vec(), node: EntryNode::Dir, extra: Extra::default() });
        for (c, name) in ["x", "y", "z"].iter().enumerate() {
            let extra = Extra { mode: Some(0o600 + c as u32), ..Extra::default() };
            tree.entries.push(Entry {
                parent: Some(0),
                name: name.as_bytes().to_vec(),
                node: EntryNode::File(c),
                extra,
            });
        }
        let contents = [(0, 0), (1, 0), (2, 0)];
        let seg = Segment { f: &f, leaves: &leaves, tree: &tree, contents: &contents };
        (Encoder::new(Level::Zstd(3)).encode_segment(&seg).unwrap().0, want)
    }

    fn written(c: &[u8], t: &Tree) -> (Vec<u8>, Rewritten) {
        let mut out = Vec::new();
        let r = rewrite(c, t, Level::Zstd(3), 1, &mut out).unwrap();
        assert_eq!(r.len, out.len() as u64);
        (out, r)
    }

    /// Written again whole, an archive is the same, byte for byte.
    #[test]
    fn whole_the_same() {
        let (c, _) = grouped();
        let t = open(&c, &natives()).unwrap().tree().unwrap().clone();
        let (out, r) = written(&c, &t);
        assert_eq!(out, c);
        assert_eq!((r.leaves_left, r.non_leaf_left, r.non_leaf_packed, r.formulas_made), (0, 0, 0, 0));
    }

    /// The first leaf taken away: the stream it alone slices left out, the formula block after
    /// it made again of its ordinals; the files left the same, at the paths of the tree.
    #[test]
    fn ordinals_made_again() {
        let (c, want) = grouped();
        let mut t = open(&c, &natives()).unwrap().tree().unwrap().clone();
        let x = t.entries.iter().position(|e| e.name == b"x").unwrap();
        t.entries.remove(x);
        // (and one moved to the top, renamed)
        let y = t.entries.iter().position(|e| e.name == b"y").unwrap();
        (t.entries[y].parent, t.entries[y].name) = (None, b"w".to_vec());
        let (out, r) = written(&c, &t);
        assert_eq!((r.leaves_left, r.non_leaf_left, r.non_leaf_packed, r.formulas_made), (1, 1, 1, 1));
        assert_eq!(r.made, [0]);
        assert!(out.len() < c.len());
        let Ok(Value::Tree(got, files)) = decode_value(&out, &natives()) else { panic!() };
        let by_name = |n: &[u8]| {
            let e = got.entries.iter().find(|e| e.name == n).unwrap();
            let EntryNode::File(k) = e.node else { panic!() };
            (e.parent.map(|p| got.entries[p].name.clone()), files[k].clone(), e.extra.mode)
        };
        assert_eq!(by_name(b"w"), (None, want[1].clone(), Some(0o601)));
        assert_eq!(by_name(b"z"), (Some(b"d".to_vec()), want[2].clone(), Some(0o602)));
        assert_eq!(got.entries.len(), 3);
    }

    /// The second leaf taken away: the stream solid with the first made again of the bytes the
    /// first slices alone, about a thirtieth of it; the first leaf's slices made again into it.
    #[test]
    fn solid_packed() {
        let (c, want) = grouped();
        let mut t = open(&c, &natives()).unwrap().tree().unwrap().clone();
        let y = t.entries.iter().position(|e| e.name == b"y").unwrap();
        t.entries.remove(y);
        let (out, r) = written(&c, &t);
        assert_eq!((r.leaves_left, r.non_leaf_left, r.non_leaf_packed, r.formulas_made), (1, 0, 1, 1));
        // (the second stream, of 30,000 bytes, down to the 1,000 the first leaf slices of it)
        assert!(c.len() - out.len() > 5000, "{} -> {}", c.len(), out.len());
        let Ok(Value::Tree(got, files)) = decode_value(&out, &natives()) else { panic!() };
        let names: Vec<&[u8]> = got.entries.iter().map(|e| &e.name[..]).collect();
        assert_eq!(names, [&b"d"[..], b"x", b"z"]);
        assert_eq!(files, [want[0].clone(), want[2].clone()]);
    }

    /// Nothing left, and contents not of the archive, are errors.
    #[test]
    fn refused() {
        let (c, _) = grouped();
        let mut out = Vec::new();
        assert!(rewrite(&c, &Tree::default(), Level::Zstd(3), 1, &mut out).is_err());
        let mut t = open(&c, &natives()).unwrap().tree().unwrap().clone();
        t.entries[1].node = EntryNode::File(9);
        assert!(rewrite(&c, &t, Level::Zstd(3), 1, &mut out).is_err());
    }
}
