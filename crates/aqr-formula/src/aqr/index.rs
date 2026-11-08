//! Archives opened by what tells where their blocks are ([`super::open_indexed`]; spec, "Partial
//! access"): the footer, and the metadata blocks back from it. The blocks of the leaves taken
//! are read as they are evaluated ([`Index::blocks_for`]): each leaf block where its files tell,
//! the module, resource and dictionary blocks where the metadata block tells, and the non-leaf
//! blocks a formula block refers to by their ordinals among those after it, found by reading
//! the blocks after it one after another (their generic structure alone) as far as the last of
//! them. What a walk over the blocks finds is kept, so that the formula blocks of a parcel of
//! several, each walking over those after it, read each block once.
//!
//! Where every leaf is evaluated, or the archive is told of as a whole, its blocks are all read
//! as [`super::open`] reads them ([`Index::parsed`]), and checked to be those its metadata tells.

use std::collections::{BTreeMap, HashMap};
use std::sync::OnceLock;

use aqr_common::varint;

use super::tree::{self, Read, Status, Tree};
use super::{
    BlockSizes, Blocks, ContentRef, Counts, Decoded, Failure, HEADER_MAGIC, LeafBlock, Payload, Resource, SIZE_HINT,
    Segmented, critical, decoded, decoded_with, defined, dict, invalid, leb, opened, resource, tree_of,
};

/// A segment's blocks as its metadata block tells where they are.
pub(super) struct SegmentIndex {
    /// Where its blocks may start, and where its metadata block is (where they end).
    start: usize,
    end: usize,
    module_at: Vec<usize>,
    resource_at: Vec<usize>,
    dictionary_at: Vec<usize>,
    /// The module, resource and dictionary blocks of the segments before it.
    first: Counts,
}

/// An archive's blocks as its metadata blocks tell where they are.
pub(super) struct Index<'c> {
    p: Payload<'c>,
    /// The threads its payload is decrypted on where it is read whole.
    threads: usize,
    segments: Vec<SegmentIndex>,
    /// Where each leaf block is (in their order, of all segments), and its segment.
    leaf_at: Vec<usize>,
    leaf_segment: Vec<usize>,
    /// The dictionary each leaf block's files tell (1-based in its segment, 0 none; none where
    /// only forks are at it).
    leaf_dictionary: Vec<Option<u64>>,
    /// The blocks read whole, once all of them are wanted ([`Index::parsed`]).
    parsed: OnceLock<Decoded<Blocks<'c>>>,
}

/// What [`open`] makes of an archive: its index, its tree and contents, and its status.
pub(super) type Indexed<'c> = (Index<'c>, Option<(Tree, Vec<ContentRef>)>, Option<Status>);

/// The block of `p` at `at`, ending by `end`, by the generic structure: its type, its data and
/// where it ends (after its padding).
fn frame<'c>(p: Payload<'c>, at: usize, end: usize) -> Decoded<(u64, &'c [u8], usize)> {
    // (the type and the size field: 10 bytes of LEB128 and 8 at most)
    if at >= end {
        return invalid("truncated block");
    }
    let head = p.get(at..end.min(at + 18))?;
    let mut q = 0;
    let ty = leb(head, &mut q)?;
    if q != varint::len(ty) {
        return invalid("a block type not in the shortest encoding");
    }
    let width = 1 << (ty % 4);
    let size = head.get(q..q + width).ok_or_else(|| Failure::Invalid("truncated block".into()))?;
    let size = u64::from_le_bytes(std::array::from_fn(|i| size.get(i).copied().unwrap_or(0)));
    let start = at + q + width;
    let n = usize::try_from(size)
        .ok()
        .filter(|&n| start.checked_add(n).is_some_and(|e| e <= end))
        .ok_or_else(|| Failure::Invalid("truncated block".into()))?;
    let data = p.get(start..start + n)?;
    let mut next = start + n;
    if (next - at) % 2 == 1 {
        next += 1;
        if next > end {
            return invalid("truncated block");
        }
    }
    Ok((ty, data, next))
}

/// Archive `p` (of the footer's fields from `fields_start`, and `size_hint`) indexed by its
/// metadata blocks, the last at `metadata` (the footer's backward offset, not 0).
pub(super) fn open(
    p: Payload<'_>,
    threads: usize,
    fields_start: usize,
    archive_end: usize,
    size_hint: u64,
    metadata: u64,
) -> Decoded<Indexed<'_>> {
    if p.sealed() {
        let (ty, data, _) = frame(p, HEADER_MAGIC.len(), archive_end)?;
        let mut q = 0;
        if ty != SIZE_HINT {
            return invalid("an encrypted archive's payload without a size hint block");
        }
        if leb(data, &mut q)? != size_hint || q != data.len() {
            return invalid("bad size hint block");
        }
    }
    // the metadata blocks, from the last back to the first: where each is and ends, and what it
    // tells
    let mut metas = Vec::new();
    let mut at = usize::try_from(metadata)
        .ok()
        .and_then(|b| fields_start.checked_sub(b))
        .filter(|&at| at >= HEADER_MAGIC.len())
        .ok_or_else(|| Failure::Invalid("no metadata block where the footer points".into()))?;
    loop {
        let (ty, data, next) = frame(p, at, archive_end)?;
        if !(0x10..=0x17).contains(&ty) {
            return invalid(match metas.is_empty() {
                true => "no metadata block where the footer points",
                false => "a metadata block not pointing to the one before it",
            });
        }
        let m = tree::read(&decoded(data, ty & 0x04 != 0, true)?)?;
        let prev = usize::try_from(m.prev).ok().and_then(|b| at.checked_sub(b));
        metas.push((at, next, m));
        match prev {
            Some(p) if p >= HEADER_MAGIC.len() => at = p,
            Some(p) if p == HEADER_MAGIC.len() - 1 => break,
            _ => return invalid("a first metadata block not pointing to the header"),
        }
    }
    metas.reverse();
    // each segment's blocks, and the leaf blocks of all (in order)
    let mut segments: Vec<SegmentIndex> = Vec::with_capacity(metas.len());
    let mut leaves: Vec<(usize, usize)> = Vec::new();
    let mut first = Counts::default();
    for (k, (at, _, m)) in metas.iter().enumerate() {
        let (at, start) = (*at, if k == 0 { HEADER_MAGIC.len() } else { metas[k - 1].1 });
        let within = |o: u64| usize::try_from(o).ok().and_then(|o| at.checked_sub(o)).filter(|&b| b >= start);
        let places = |offsets: &[u64]| -> Decoded<Vec<usize>> {
            let at: Option<Vec<usize>> = offsets.iter().map(|&o| within(o)).collect();
            match at {
                Some(at) if at.is_sorted_by(|a, b| a < b) => Ok(at),
                _ => invalid("module, resource or dictionary offsets not those of the blocks"),
            }
        };
        let s = SegmentIndex {
            start,
            end: at,
            module_at: places(&m.modules)?,
            resource_at: places(&m.resources)?,
            dictionary_at: places(&m.dictionaries)?,
            first,
        };
        let files = m.entries.iter().filter_map(|(_, _, r)| match *r {
            Read::File { offset, .. } => Some(offset),
            _ => None,
        });
        let forks = m.forks.iter().flatten().map(|(_, f)| f.0);
        for o in files.chain(forks) {
            match within(o) {
                Some(b) => leaves.push((b, k)),
                None => return invalid("a file or a fork not at a leaf block"),
            }
        }
        first = Counts {
            leaves: 0,
            non_leaf: 0,
            modules: first.modules + s.module_at.len(),
            resources: first.resources + s.resource_at.len(),
            dictionaries: first.dictionaries + s.dictionary_at.len(),
        };
        segments.push(s);
    }
    leaves.sort_unstable();
    leaves.dedup();
    let (leaf_at, leaf_segment): (Vec<usize>, Vec<usize>) = leaves.into_iter().unzip();
    let mut leaf_dictionary = vec![None; leaf_at.len()];
    // each segment's tree, then merged (spec, "Interpreting an archive")
    let mut trees = Vec::with_capacity(metas.len());
    let mut prev = HEADER_MAGIC.len() - 1;
    let mut status = None;
    for (at, _, m) in metas {
        status = Some(m.status);
        let mut leaf_of = |b: usize, dictionary: Option<u64>| -> Decoded<Option<(usize, Option<u64>)>> {
            let Ok(l) = leaf_at.binary_search(&b) else { return Ok(None) };
            if let Some(d) = dictionary {
                match leaf_dictionary[l] {
                    None => leaf_dictionary[l] = Some(d),
                    Some(e) if e != d => return invalid("files of a leaf block of other dictionaries"),
                    Some(_) => {}
                }
            }
            Ok(Some((l, None)))
        };
        trees.push(tree_of(m, at, prev, &mut leaf_of, leaf_at.len(), None)?);
        prev = at;
    }
    let tree = match trees.len() {
        1 => trees.pop().map(|t| (t.tree, t.refs)),
        _ => Some(tree::merge(trees)),
    };
    let index = Index { p, threads, segments, leaf_at, leaf_segment, leaf_dictionary, parsed: OnceLock::new() };
    Ok((index, tree, status))
}

/// The module, resource and dictionary blocks read as the leaves taken need them, by index
/// among all: each as it decodes, or its failure.
#[derive(Default)]
struct Auxiliary<'c> {
    modules: HashMap<usize, Stored<'c, Vec<u8>>>,
    resources: HashMap<usize, Stored<'c, Resource>>,
    dictionaries: HashMap<usize, Decoded<dict::Dictionary>>,
}

/// A block decoded, and its data as stored; or its failure.
type Stored<'c, T> = Decoded<(T, &'c [u8])>;

/// A walk over the blocks of a segment from where its formula blocks end: of each walk, from
/// where it started, where it ended and the non-leaf items it went over.
type Walked = BTreeMap<usize, (usize, usize)>;

impl<'c> Index<'c> {
    /// The number of leaf blocks.
    pub(super) fn leaves_len(&self) -> usize {
        self.leaf_at.len()
    }

    /// The number of module blocks.
    pub(super) fn modules_len(&self) -> usize {
        self.segments.last().map_or(0, |s| s.first.modules + s.module_at.len())
    }

    /// The archive's blocks read whole, as [`super::open`] reads them: an invalid archive where
    /// they are not those its metadata blocks tell, of contents `refs` (of the tree, by leaf).
    pub(super) fn parsed(&self, refs: Option<&Vec<ContentRef>>) -> Decoded<&Blocks<'c>> {
        let r = self.parsed.get_or_init(|| {
            let (blocks, tree, _) = opened(self.p.whole(self.threads)?, self.p.sealed(), false)?;
            let leaves_at = blocks.leaf_at == self.leaf_at;
            if !leaves_at || tree.map(|t| t.1).as_ref() != refs {
                return invalid("leaf blocks not those the metadata blocks tell");
            }
            Ok(blocks)
        });
        r.as_ref().map_err(Clone::clone)
    }

    /// The segment of the block at `at` (which is in one) and its dictionaries before it: those
    /// from the first of the segment to the one before `at`, by index among all.
    fn dictionaries_before(&self, s: usize, at: usize) -> (usize, usize) {
        let seg = &self.segments[s];
        let first = seg.first.dictionaries;
        (first, first + seg.dictionary_at.partition_point(|&d| d < at))
    }

    /// Dictionary `d` (by index among all, of segment `s`) decoded, once.
    fn dictionary(&self, s: usize, d: usize, aux: &mut Auxiliary<'c>) -> Decoded<()> {
        let seg = &self.segments[s];
        let r = aux.dictionaries.entry(d).or_insert_with(|| {
            let at = seg.dictionary_at[d - seg.first.dictionaries];
            match frame(self.p, at, seg.end)? {
                (0x50, ..) => resource("registered dictionaries are not supported yet"),
                (ty @ 0x51..=0x57, data, _) => dict::Dictionary::read(decoded(data, true, true)?, ty & 0x04 != 0),
                _ => invalid("module, resource or dictionary offsets not those of the blocks"),
            }
        });
        r.as_ref().map(|_| ()).map_err(Clone::clone)
    }

    /// Module `m` (by index among all, of segment `s`) decoded, once.
    fn module(&self, s: usize, m: usize, aux: &mut Auxiliary<'c>) -> Decoded<()> {
        let seg = &self.segments[s];
        let r = aux.modules.entry(m).or_insert_with(|| {
            let at = seg.module_at[m - seg.first.modules];
            match frame(self.p, at, seg.end)? {
                (0x60, ..) => resource("registered modules are not supported yet"),
                (0x61..=0x63, data, _) => Ok((decoded(data, true, true)?, data)),
                _ => invalid("module, resource or dictionary offsets not those of the blocks"),
            }
        });
        r.as_ref().map(|_| ()).map_err(Clone::clone)
    }

    /// Resource `r` (by index among all, of segment `s`) decoded, once.
    fn resource(&self, s: usize, r: usize, aux: &mut Auxiliary<'c>) -> Decoded<()> {
        let seg = &self.segments[s];
        let d = aux.resources.entry(r).or_insert_with(|| {
            let at = seg.resource_at[r - seg.first.resources];
            match frame(self.p, at, seg.end)? {
                (0x64..=0x67, data, _) => Ok((Resource::new(decoded(data, true, true)?), data)),
                _ => invalid("module, resource or dictionary offsets not those of the blocks"),
            }
        });
        d.as_ref().map(|_| ()).map_err(Clone::clone)
    }

    /// The data of a block of segment `s` at `at` past its dictionary index where it has one
    /// (`has`), and its dictionary (decoded), by index among all.
    fn dictionary_of(
        &self,
        s: usize,
        at: usize,
        d: &'c [u8],
        has: bool,
        aux: &mut Auxiliary<'c>,
    ) -> Decoded<(&'c [u8], Option<usize>)> {
        let (first, before) = self.dictionaries_before(s, at);
        let (d, dict) = super::dictionary_of(d, has, first, before)?;
        if let Some(i) = dict {
            self.dictionary(s, i, aux)?;
        }
        Ok((d, dict))
    }

    /// Walks the blocks of segment `s` from `from` (where a formula block ends) over `need`
    /// non-leaf items, putting the non-leaf blocks in `items` by where they are; along walks
    /// before (`walked`), which it takes into its own.
    fn walk(
        &self,
        s: usize,
        from: usize,
        need: usize,
        walked: &mut Walked,
        items: &mut BTreeMap<usize, (&'c [u8], bool, Option<usize>)>,
        aux: &mut Auxiliary<'c>,
    ) -> Decoded<()> {
        let end = self.segments[s].end;
        let (mut at, mut n) = (from, 0);
        while n < need {
            if let Some((to, k)) = walked.remove(&at) {
                (at, n) = (to, n + k);
                continue;
            }
            if at >= end {
                return invalid("a referenced block that does not exist");
            }
            let (ty, data, next) = frame(self.p, at, end)?;
            match ty {
                0x40..=0x4b => {
                    let (data, dict) = self.dictionary_of(s, at, data, ty & 0x08 != 0, aux)?;
                    items.insert(at, (data, ty & 0x0c != 0, dict));
                    n += 1;
                }
                _ if critical(ty) && !defined(ty) => {
                    return Err(Failure::Unsupported(format!("a block of an undefined critical type {ty:#x}")));
                }
                0x00..=0x03 if self.p.sealed() => return invalid("a spacer block in an encrypted archive's payload"),
                // spacer, leaf and auxiliary blocks, mirror blocks and undefined non-critical blocks
                0x00..=0x03 | 0x20..=0x37 | 0x50..=0x57 | 0x60..=0x67 | 0x70..=0x77 => {}
                _ if !critical(ty) => {}
                0x10..=0x17 => return invalid("a metadata block within a segment"),
                _ => return invalid(format!("bad block of type {ty:#x}")),
            }
            at = next;
        }
        walked.insert(from, (at, n));
        Ok(())
    }

    /// Leaf `l` read: its block, and where it is a formula block, the blocks it refers to (the
    /// non-leaf blocks into `items`); and where its non-leaf items start, of a formula block.
    #[allow(clippy::type_complexity)]
    fn leaf(
        &self,
        l: usize,
        walked: &mut [Walked],
        items: &mut BTreeMap<usize, (&'c [u8], bool, Option<usize>)>,
        formulas: &mut Vec<(&'c [u8], Option<usize>)>,
        decoded_formulas: &mut Vec<Option<Vec<u8>>>,
        aux: &mut Auxiliary<'c>,
    ) -> Decoded<(LeafBlock<'c>, Option<usize>)> {
        let (at, s) = (self.leaf_at[l], self.leaf_segment[l]);
        let seg = &self.segments[s];
        let (ty, data, next) = frame(self.p, at, seg.end)?;
        // (its dictionary, as its files tell)
        let told = |dict: Option<usize>| {
            let own = dict.map_or(0, |d| (d - seg.first.dictionaries) as u64 + 1);
            match self.leaf_dictionary[l] {
                Some(d) if d != own => invalid("a file's dictionary not that of its leaf block"),
                _ => Ok(()),
            }
        };
        match ty {
            0x20..=0x37 => {
                let (data, dict) = self.dictionary_of(s, at, data, ty & 0x10 != 0, aux)?;
                told(dict)?;
                Ok((LeafBlock::Stored(data, ty & 0x18 != 0, ty & 0x04 != 0, dict), None))
            }
            0x70..=0x77 => {
                let (data, dict) = self.dictionary_of(s, at, data, ty & 0x04 != 0, aux)?;
                told(dict)?;
                let mut q = 0;
                let outputs = leb(data, &mut q)?;
                if outputs == 0 {
                    return invalid("a formula block of no outputs");
                }
                let data = &data[q..];
                // the lists of what it refers to, before its instructions
                let d = {
                    let dictionary = match dict {
                        Some(i) => Some(aux.dictionaries[&i].as_ref().expect("decoded")),
                        None => None,
                    };
                    decoded_with(data, true, true, dictionary)?
                };
                let mut r = super::Reader { d: &d, at: 0 };
                let deltas = r.nums()?;
                let (res, mods) = (r.nums()?, r.nums()?);
                let need = deltas.iter().try_fold(0u64, |n, &d| n.checked_add(d)?.checked_add(1));
                let need = need.and_then(|n| usize::try_from(n).ok()).unwrap_or(usize::MAX);
                let modules = seg.module_at.partition_point(|&m| m < at);
                let resources = seg.resource_at.partition_point(|&r| r < at);
                // (those out of range left to be told of as the formula block is read)
                for &m in mods.iter().filter(|&&m| m < modules as u64) {
                    self.module(s, seg.first.modules + m as usize, aux)?;
                }
                for &r in res.iter().filter(|&&r| r < resources as u64) {
                    self.resource(s, seg.first.resources + r as usize, aux)?;
                }
                self.walk(s, next, need, &mut walked[s], items, aux)?;
                formulas.push((data, dict));
                decoded_formulas.push(Some(d));
                let leaf = LeafBlock::Formula {
                    outputs,
                    data: formulas.len() - 1,
                    modules: seg.first.modules + modules,
                    resources: seg.first.resources + resources,
                    // (where its non-leaf items start, made an index once all are found)
                    non_leaf: 0,
                    segment: s,
                };
                Ok((leaf, Some(next)))
            }
            _ => invalid("a file or a fork not at a leaf block"),
        }
    }

    /// The blocks of leaves `picked` (by index, in their order) read, as [`Blocks`] of all the
    /// leaves (the others not read) and of the module, resource, dictionary and non-leaf blocks
    /// that those refer to (the others empty); a leaf whose blocks do not read is its failure.
    pub(super) fn blocks_for(&self, picked: &[usize]) -> Blocks<'c> {
        let mut leaves: Vec<LeafBlock<'c>> = (0..self.leaf_at.len()).map(|_| LeafBlock::Unread).collect();
        let mut walked: Vec<Walked> = (0..self.segments.len()).map(|_| Walked::new()).collect();
        let mut items = BTreeMap::new();
        let (mut formulas, mut decoded_formulas) = (Vec::new(), Vec::new());
        let mut aux = Auxiliary::default();
        let mut firsts = Vec::new();
        // (the last first: a formula block's walk takes those of the formula blocks after it)
        for &l in picked.iter().rev() {
            leaves[l] = match self.leaf(l, &mut walked, &mut items, &mut formulas, &mut decoded_formulas, &mut aux) {
                Ok((leaf, first)) => {
                    firsts.extend(first.map(|f| (l, f)));
                    leaf
                }
                Err(e) => LeafBlock::Bad(e),
            };
        }
        let non_leaf_at: Vec<usize> = items.keys().copied().collect();
        for (l, f) in firsts {
            if let LeafBlock::Formula { non_leaf, .. } = &mut leaves[l] {
                *non_leaf = non_leaf_at.partition_point(|&b| b < f);
            }
        }
        let segments = self
            .segments
            .iter()
            .map(|s| {
                let counts = |non_leaf, modules, resources, dictionaries| Counts {
                    leaves: 0,
                    non_leaf,
                    modules,
                    resources,
                    dictionaries,
                };
                let first = counts(
                    non_leaf_at.partition_point(|&b| b < s.start),
                    s.first.modules,
                    s.first.resources,
                    s.first.dictionaries,
                );
                let end = counts(
                    non_leaf_at.partition_point(|&b| b < s.end),
                    s.first.modules + s.module_at.len(),
                    s.first.resources + s.resource_at.len(),
                    s.first.dictionaries + s.dictionary_at.len(),
                );
                Segmented { first, end, metadata: None }
            })
            .collect();
        let last = self.segments.last().map_or(Counts::default(), |s| Counts {
            modules: s.first.modules + s.module_at.len(),
            resources: s.first.resources + s.resource_at.len(),
            dictionaries: s.first.dictionaries + s.dictionary_at.len(),
            ..Counts::default()
        });
        let (mut modules, mut module_data) = (Vec::with_capacity(last.modules), Vec::with_capacity(last.modules));
        for m in 0..last.modules {
            let (w, d) = match aux.modules.remove(&m) {
                Some(Ok((w, d))) => (w, d),
                _ => (Vec::new(), &[][..]),
            };
            modules.push(w);
            module_data.push(d);
        }
        let (mut resources, mut resource_data) = (Vec::new(), Vec::new());
        for r in 0..last.resources {
            let (r, d) = match aux.resources.remove(&r) {
                Some(Ok((r, d))) => (r, d),
                _ => (Resource::new(Vec::new()), &[][..]),
            };
            resources.push(r);
            resource_data.push(d);
        }
        let dictionaries = (0..last.dictionaries)
            .map(|d| match aux.dictionaries.remove(&d) {
                Some(Ok(d)) => d,
                _ => dict::Dictionary::Raw(Vec::new()),
            })
            .collect();
        let at_of = |f: fn(&SegmentIndex) -> &Vec<usize>| self.segments.iter().flat_map(f).copied().collect();
        Blocks {
            leaves,
            leaf_at: self.leaf_at.clone(),
            module_at: at_of(|s| &s.module_at),
            resource_at: at_of(|s| &s.resource_at),
            dictionary_at: at_of(|s| &s.dictionary_at),
            leaf_dictionary: vec![None; self.leaf_at.len()],
            segments,
            formulas,
            decoded_formulas,
            non_leaf: items.into_values().collect(),
            non_leaf_at,
            modules,
            resources,
            module_data,
            resource_data,
            dictionaries,
            sizes: BlockSizes::default(),
        }
    }
}
