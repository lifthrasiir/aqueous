//! The functions of two versions of a program's code matched, for the targets of one to be
//! written as the other's (`arm64::remap`, `x86::remap`; after Courgette). The code is a
//! sequence of units (words or bytes) with its references (targets of calls and the like, and
//! addresses) cleared, cut into functions where calls go; the references at the same places of
//! the functions matched vote for the map of the image's values to the reference's.

use alloc::vec;
use alloc::vec::Vec;

/// Code as [`map`] takes it ([`Code::new`]).
pub(crate) struct Code<T> {
    /// The units, references cleared.
    pub units: Vec<T>,
    /// Where the functions start, in order (the first 0), and the end.
    pub cuts: Vec<usize>,
    /// Of each function, the value of a reference to its start (of space 0).
    pub starts: Vec<u64>,
    /// The references: their value, the unit they start at (in order, one each at most) and
    /// their space (16 bytes each).
    refs: Vec<(u64, u32, u8)>,
    /// Of each function, the hash of its units, its start and its end.
    funcs: Vec<(u64, usize, usize)>,
}

impl<T: Copy + Eq + Into<u64>> Code<T> {
    /// Of `units` (references cleared), the functions' `cuts` and `starts`, and the references
    /// (each the unit it starts at, in order, one each at most; its space; its value).
    pub fn new(units: Vec<T>, cuts: Vec<usize>, starts: Vec<u64>, refs: Vec<(usize, u8, u64)>) -> Code<T> {
        assert!(units.len() <= u32::MAX as usize, "code of 4 GiB at most");
        let refs = refs.into_iter().map(|(u, s, v)| (v, u as u32, s)).collect();
        let mut c = Code { units, cuts, starts, refs, funcs: Vec::new() };
        c.funcs = c.functions();
        c
    }

    /// Of each function, the hash of its units, its start and its end.
    fn functions(&self) -> Vec<(u64, usize, usize)> {
        self.cuts
            .windows(2)
            .map(|c| {
                let mut h = 0xcbf2_9ce4_8422_2325u64 ^ (c[1] - c[0]) as u64;
                for &u in &self.units[c[0]..c[1]] {
                    h = (h.rotate_left(23) ^ u.into()).wrapping_mul(0x9e37_79b9_7f4a_7c15);
                }
                (h, c[0], c[1])
            })
            .collect()
    }

    /// The references from unit `from` to `to`.
    /// The references from unit `from` to `to`: each its unit, its space and its value.
    fn refs_in(&self, from: usize, to: usize) -> impl Iterator<Item = (usize, u8, u64)> + '_ {
        let a = self.refs.partition_point(|r| (r.1 as usize) < from);
        let b = self.refs.partition_point(|r| (r.1 as usize) < to);
        self.refs[a..b].iter().map(|&(v, u, s)| (u as usize, s, v))
    }

    fn ref_at(&self, unit: usize) -> Option<(u8, u64)> {
        let i = self.refs.binary_search_by_key(&unit, |r| r.1 as usize).ok()?;
        Some((self.refs[i].2, self.refs[i].0))
    }
}

/// What [`map`] found.
#[derive(Debug, Default, Clone, Copy)]
pub struct Stats {
    pub functions: [usize; 2],
    pub matched: usize,
    /// Units of the reference's functions matched.
    pub matched_units: usize,
    pub units: [usize; 2],
    /// Entries of the maps, of each space (the first two).
    pub mapped: [usize; 2],
}

/// The entries of `f` (by hash first) whose hash no other has.
fn unique(mut f: Vec<(u64, usize)>) -> Vec<(u64, usize)> {
    f.sort_unstable();
    let mut out = Vec::with_capacity(f.len());
    let mut i = 0;
    while i < f.len() {
        let j = i + f[i..].iter().take_while(|x| x.0 == f[i].0).count();
        if j == i + 1 {
            out.push(f[i]);
        }
        i = j;
    }
    out
}

/// Of `votes` (each a value of the reference and one of the image), the image's value most
/// voted for each of the reference's, by the reference's.
fn majority(votes: &mut [(u64, u64)]) -> Vec<(u64, u64)> {
    votes.sort_unstable();
    tally(votes)
}

/// [`majority`] of `votes` sorted.
fn tally(votes: &[(u64, u64)]) -> Vec<(u64, u64)> {
    let mut fwd: Vec<(u64, u64)> = Vec::new();
    let mut i = 0;
    while i < votes.len() {
        let a = votes[i].0;
        let (mut best, mut n) = ((0, 0), 0);
        while i < votes.len() && votes[i].0 == a {
            let j = i + votes[i..].iter().take_while(|v| **v == votes[i]).count();
            if j - i > n {
                (best, n) = (votes[i], j - i);
            }
            i = j;
        }
        fwd.push(best);
    }
    fwd
}

/// Of `fwd` (the reference's values to the image's), the image's values that one value of the
/// reference maps to, with it, by the image's.
fn inverse(fwd: &[(u64, u64)]) -> Vec<(u64, u64)> {
    let mut inv: Vec<(u64, u64)> = fwd.iter().map(|&(a, b)| (b, a)).collect();
    inv.sort_unstable();
    let mut out = Vec::with_capacity(inv.len());
    let mut i = 0;
    while i < inv.len() {
        let j = i + inv[i..].iter().take_while(|x| x.0 == inv[i].0).count();
        if j == i + 1 {
            out.push(inv[i]);
        }
        i = j;
    }
    out
}

/// `a` and `b`, sorted, one sequence sorted.
fn merge(a: &[(u64, u64)], b: &[(u64, u64)]) -> Vec<(u64, u64)> {
    let mut out = Vec::with_capacity(a.len() + b.len());
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        if a[i] <= b[j] {
            out.push(a[i]);
            i += 1;
        } else {
            out.push(b[j]);
            j += 1;
        }
    }
    out.extend_from_slice(&a[i..]);
    out.extend_from_slice(&b[j..]);
    out
}

/// Rounds of matching after the first, at most.
const ROUNDS: usize = 16;

/// The maps of the image's values to the reference's, of each of `spaces` spaces, by the
/// image's. Functions whose units hash alike and uniquely in both, of one length, are matched,
/// and the references at the same places in them (and their starts) vote. Then, in rounds, a
/// function not matched yet is matched to the one that its start maps to by the votes so far,
/// or else to the one its matched neighbours' shift tells; their units from the start and from
/// the end, alike as far as they go, vote. (Asking those to be some share of the units, or
/// aligning the units between them, maps fewer.) Values of space 0 are modulo `mask + 1`.
pub(crate) fn map<T: Copy + Eq + Into<u64>>(
    a: &Code<T>,
    b: &Code<T>,
    spaces: usize,
    mask: u64,
) -> (Vec<Vec<(u64, u64)>>, Stats) {
    let (fa, fb) = (&a.funcs, &b.funcs);
    let mut stats =
        Stats { functions: [fa.len(), fb.len()], units: [a.units.len(), b.units.len()], ..Stats::default() };
    let mut votes: Vec<Vec<(u64, u64)>> = vec![Vec::new(); spaces];
    // (of those, the votes for the starts of the reference's functions apart, which the rounds
    // go by: sorted, and those since)
    let mut sorted_starts = a.starts.clone();
    sorted_starts.sort_unstable();
    let mut starts: Vec<(u64, u64)> = Vec::new();
    let mut fresh: Vec<(u64, u64)> = Vec::new();
    // the references of `len` units from `x` of the reference and `y` of the image
    let vote = |votes: &mut Vec<Vec<(u64, u64)>>, fresh: &mut Vec<(u64, u64)>, x: usize, y: usize, len: usize| {
        for (u, s, v) in a.refs_in(x, x + len) {
            if let Some((t, w)) = b.ref_at(y + (u - x)).filter(|r| r.0 == s) {
                votes[t as usize].push((v, w));
                if t == 0 && sorted_starts.binary_search(&v).is_ok() {
                    fresh.push((v, w));
                }
            }
        }
    };
    // of each function of the reference, the one of the image matched
    let mut pair: Vec<Option<usize>> = vec![None; fa.len()];
    let mut taken = vec![false; fb.len()];
    let (ua, ub) = (
        unique(fa.iter().enumerate().map(|(i, f)| (f.0, i)).collect()),
        unique(fb.iter().enumerate().map(|(i, f)| (f.0, i)).collect()),
    );
    let mut j = 0;
    for &(h, i) in &ua {
        while j < ub.len() && ub[j].0 < h {
            j += 1;
        }
        let Some(&(_, k)) = ub.get(j).filter(|x| x.0 == h && fb[x.1].2 - fb[x.1].1 == fa[i].2 - fa[i].1) else {
            continue;
        };
        (pair[i], taken[k]) = (Some(k), true);
        votes[0].push((a.starts[i], b.starts[k]));
        fresh.push((a.starts[i], b.starts[k]));
        vote(&mut votes, &mut fresh, fa[i].1, fb[k].1, fa[i].2 - fa[i].1);
    }
    // the functions of the image by their starts
    let mut by_start: Vec<(u64, usize)> = b.starts.iter().enumerate().map(|(k, &s)| (s, k)).collect();
    by_start.sort_unstable();
    let at = |s: u64| by_start.binary_search_by_key(&s, |x| x.0).ok().map(|j| by_start[j].1);
    for _ in 0..ROUNDS {
        fresh.sort_unstable();
        starts = merge(&starts, &fresh);
        fresh.clear();
        let fwd = tally(&starts);
        let shift = |i: usize| pair[i].map(|k| b.starts[k].wrapping_sub(a.starts[i]) & mask);
        // the shift of the last function matched before each, and of the next after
        let mut before = vec![None; fa.len()];
        let mut after = vec![None; fa.len()];
        for i in 1..fa.len() {
            before[i] = shift(i - 1).or(before[i - 1]);
        }
        for i in (0..fa.len().saturating_sub(1)).rev() {
            after[i] = shift(i + 1).or(after[i + 1]);
        }
        let mut new = 0;
        for i in 0..fa.len() {
            if pair[i].is_some() {
                continue;
            }
            let (a0, a1, s) = (fa[i].1, fa[i].2, a.starts[i]);
            let voted = fwd.binary_search_by_key(&s, |x| x.0).ok().map(|j| fwd[j].1);
            let near = before[i].filter(|_| before[i] == after[i]).map(|d| s.wrapping_add(d) & mask);
            let Some(k) = voted.or(near).and_then(at).filter(|&k| !taken[k]) else { continue };
            let (b0, b1) = (fb[k].1, fb[k].2);
            let same = |(i, k): (usize, usize)| a.units[i] == b.units[k];
            let head = (a0..a1).zip(b0..b1).take_while(|&p| same(p)).count();
            let tail = (a0 + head..a1).rev().zip((b0 + head..b1).rev()).take_while(|&p| same(p)).count();
            (pair[i], taken[k]) = (Some(k), true);
            new += 1;
            votes[0].push((s, b.starts[k]));
            fresh.push((s, b.starts[k]));
            vote(&mut votes, &mut fresh, a0, b0, head);
            vote(&mut votes, &mut fresh, a1 - tail, b1 - tail, tail);
        }
        if new == 0 {
            break;
        }
    }
    for (i, p) in pair.iter().enumerate() {
        if p.is_some() {
            stats.matched += 1;
            stats.matched_units += fa[i].2 - fa[i].1;
        }
    }
    // (values mapped to themselves too: their flags then go with their neighbours', which costs
    // less than the maps' entries saved)
    let maps: Vec<_> = votes.into_iter().map(|mut v| inverse(&majority(&mut v))).collect();
    for (m, n) in stats.mapped.iter_mut().zip(&maps) {
        *m = n.len();
    }
    (maps, stats)
}

/// `maps` (of an image to its reference) composed with those of the reference to an image of
/// its own (`parent`'s, by the reference's values): to that image's values, which the
/// reference is written in. Values that two of the image's would map to are left unmapped.
pub(crate) fn compose(maps: &[Vec<(u64, u64)>], parent: impl Fn(usize, u64) -> Option<u64>) -> Vec<Vec<(u64, u64)>> {
    maps.iter()
        .enumerate()
        .map(|(i, m)| {
            let mut m: Vec<(u64, u64)> = m.iter().map(|&(b, a)| (b, parent(i, a).unwrap_or(a))).collect();
            m.sort_unstable_by_key(|x| x.1);
            let mut keep = vec![true; m.len()];
            for j in 1..m.len() {
                if m[j].1 == m[j - 1].1 {
                    (keep[j - 1], keep[j]) = (false, false);
                }
            }
            let mut m: Vec<_> = m.into_iter().zip(keep).filter(|x| x.1).map(|x| x.0).collect();
            m.sort_unstable();
            m
        })
        .collect()
}

/// A sample of the functions of an image, to tell images alike: the hashes of those that
/// hash to a multiple of [`SAMPLE`].
#[derive(Debug, Clone)]
pub struct Signature(Vec<u64>);

/// What hashes of [`Signature`]s are multiples of.
pub const SAMPLE: u64 = 16;

impl Signature {
    pub(crate) fn of<T: Copy + Eq + Into<u64>>(code: &Code<T>) -> Signature {
        let mut h: Vec<u64> = code.funcs.iter().map(|f| f.0).filter(|h| h.is_multiple_of(SAMPLE)).collect();
        h.sort_unstable();
        h.dedup();
        Signature(h)
    }

    /// The share of the hashes of the larger of the two in both.
    pub fn alike(&self, other: &Signature) -> f64 {
        let (a, b) = (&self.0, &other.0);
        let (mut i, mut j, mut n) = (0, 0, 0);
        while i < a.len() && j < b.len() {
            match a[i].cmp(&b[j]) {
                core::cmp::Ordering::Less => i += 1,
                core::cmp::Ordering::Greater => j += 1,
                core::cmp::Ordering::Equal => (i, j, n) = (i + 1, j + 1, n + 1),
            }
        }
        n as f64 / a.len().max(b.len()).max(1) as f64
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}
