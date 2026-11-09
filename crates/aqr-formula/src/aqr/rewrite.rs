//! An archive written again of what its tree, edited, still holds ([`rewrite`]): `aqr
//! --compact`. Segment by segment, as they are (a segment of none of the tree's contents left
//! out), each block it still needs copied as it is: its module, resource and dictionary blocks
//! (all of them, as the formula blocks refer to them by their indices in the segment), the leaf
//! blocks of the tree's contents, and the non-leaf blocks those refer to. Spacer blocks, the size
//! hint block and the blocks of no meaning known are left out.
//!
//! A formula block refers to non-leaf blocks by their ordinals among those after it (spec,
//! "Formula data"): where blocks between it and those it refers to are left out, its ordinals
//! are made again and its formula data compressed again (without a dictionary); its
//! instructions are kept as they are.
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
use super::tree::{Entry, EntryNode, Extra, Status, Tree};
use super::{
    ContentAt, Encoder, Failure, HEADER_MAGIC, LeafBlock, Level, Reader, SegmentWriter, always_compressed, critical,
    decoded_with, footer_of, formula_block, frame, inspect, opened,
};
use crate::{Error, Result};

/// What [`rewrite`] wrote.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Rewritten {
    /// The bytes of the archive.
    pub len: u64,
    /// The segments, of the archive written and of the one before.
    pub segments: (usize, usize),
    /// The leaf and non-leaf blocks left out, and the formula blocks made again.
    pub leaves_left: usize,
    pub non_leaf_left: usize,
    pub formulas_made: usize,
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
/// away, moved, their extras changed), its contents those of the archive ([module](self)).
/// Fails on an archive of mirror blocks, or of metadata this crate does not read, and on a tree
/// of no entries.
pub fn rewrite(c: &[u8], t: &Tree, level: Level, threads: usize, out: &mut dyn Write) -> Result<Rewritten> {
    let fail = |f: Failure| Error::new(f.to_string());
    if !c.starts_with(&HEADER_MAGIC) {
        return Err(Error::new("not a standalone archive"));
    }
    unknown_metadata(c)?;
    let (blocks, tree, _) = opened(c, false, false).map_err(fail)?;
    let (_, refs) = tree.ok_or_else(|| Error::new("an archive of a single file, not of paths"))?;
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
    let segment_of = |leaf: usize| blocks.segments.iter().position(|s| (s.first.leaves..s.end.leaves).contains(&leaf));
    let kept: Vec<usize> = (0..blocks.segments.len())
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
    let mut r = Rewritten { segments: (kept.len().max(1), blocks.segments.len()), ..Rewritten::default() };
    let mut w = SegmentWriter::new();
    let io = |e: std::io::Error| Error::new(e.to_string());
    out.write_all(w.head()).map_err(io)?;
    let mut written = w.len() as u64;
    let mut leaf_at = vec![usize::MAX; blocks.leaves.len()];
    for n in 0..kept.len().max(1) {
        if let Some(&s) = kept.get(n) {
            let seg = &blocks.segments[s];
            let leaves = seg.first.leaves..seg.end.leaves;
            // the non-leaf blocks still referred to, and the ordinals of each formula block's
            let whole = live[leaves.clone()].iter().all(|&l| l);
            let mut keep = vec![whole; seg.end.non_leaf - seg.first.non_leaf];
            let mut programs: HashMap<usize, Referred> = HashMap::new();
            if !whole {
                for l in leaves.clone().filter(|&l| live[l]) {
                    if let Some(p) = referred(&blocks, l).map_err(fail)? {
                        p.ordinals.iter().for_each(|&o| keep[o - seg.first.non_leaf] = true);
                        programs.insert(l, p);
                    }
                }
            }
            // (of a non-leaf ordinal, those kept before it in the segment)
            let mut rank = Vec::with_capacity(keep.len() + 1);
            rank.push(0);
            for &k in &keep {
                rank.push(rank.last().unwrap() + k as usize);
            }
            let rank_of = |o: usize| rank[o - seg.first.non_leaf];
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
                                again(p, outputs, rank_of(non_leaf), &rank_of, &enc)?
                            }
                            _ => None,
                        };
                        leaf_at[l] = w.len();
                        match again {
                            Some(b) => {
                                r.formulas_made += 1;
                                (Copied::Leaf(None), Some(b))
                            }
                            None => (Copied::Leaf(dictionary), None),
                        }
                    }
                    Kind::NonLeaf(o) if !keep[o - seg.first.non_leaf] => {
                        r.non_leaf_left += 1;
                        continue;
                    }
                    Kind::NonLeaf(_) => (Copied::NonLeaf, None),
                    Kind::Dropped => continue,
                };
                let bytes = made.as_deref().unwrap_or(bytes);
                out.write_all(bytes).map_err(io)?;
                w.copied(what, bytes.len());
                written += bytes.len() as u64;
            }
        }
        // the entries of the segment
        let (st, contents) = segment_tree(t, &homes, n, &|k| {
            let (l, output, size) = refs[k];
            ContentAt { leaf: leaf_at[l], output, size }
        });
        if st.entries.is_empty() {
            return Err(Error::new("a segment of no entries (of the root's forks alone)"));
        }
        let end = match n == last {
            false => w.close_as(&enc, &st, &contents, Status::None)?,
            true => w.finish_as(&enc, &st, &contents, Status::Complete)?.0,
        };
        out.write_all(&end).map_err(io)?;
        written += end.len() as u64;
    }
    r.len = written;
    Ok(r)
}

/// Fails where archive `c` has metadata that [`Extra`] does not hold (a comment, ZIP's extra
/// fields, members of no meaning known), which writing it again would lose.
fn unknown_metadata(c: &[u8]) -> Result<()> {
    let i = inspect::inspect(c, false, &|_| None, None).map_err(|f| Error::new(f.to_string()))?;
    let known = |name: Option<&str>| name.is_some_and(|n| !matches!(n, "comment" | "zip-extras"));
    for s in &i.segments {
        if let Some(m) =
            s.archive_extra.iter().find(|m| m.key.and_then(super::archive_key_name) != Some("segment-status"))
        {
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

/// A formula block's references to non-leaf blocks ([`referred`]): their ordinals among all, its
/// formula data decoded and where the list of them ends in it.
struct Referred {
    ordinals: Vec<usize>,
    data: Vec<u8>,
    prefix: usize,
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
    Ok(Some(Referred { ordinals, data: d, prefix }))
}

/// Formula block of `outputs` outputs referring as `p` does, made again where the non-leaf
/// blocks kept before it are `first` and before each, `rank`: none where its ordinals are the
/// same.
fn again(
    p: &Referred,
    outputs: u64,
    first: usize,
    rank: &dyn Fn(usize) -> usize,
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
    // (none left out between it and those it refers to: as it is)
    let before = Reader { d, at: 0 }.nums().map_err(|f| Error::new(f.to_string()))?;
    if before == deltas {
        return Ok(None);
    }
    let mut data = Vec::with_capacity(d.len());
    varint::put(&mut data, deltas.len() as u64);
    deltas.iter().for_each(|&x| varint::put(&mut data, x));
    data.extend_from_slice(&d[p.prefix..]);
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
        assert_eq!((r.leaves_left, r.non_leaf_left, r.formulas_made), (0, 0, 0));
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
        assert_eq!((r.leaves_left, r.non_leaf_left, r.formulas_made), (1, 1, 1));
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
