//! Sets of files coded against each other: PNG files of the same geometry among the contents of
//! a formula that are alike (variants of a picture, with layers shown or hidden), as one
//! application of aqr-png's `png-set`, each file against its parent along a maximum spanning
//! tree of how alike they are.
//!
//! The sets of the PNG files among the contents made beforehand ([`crate::prepare`]) are
//! planned before any is coded ([`plan`]), so that the files with a parent are coded only
//! against it, never alone. Those of other PNG files are found among their formulas alone.
//!
//! Within a content, which is decoded whole, the tree's depth costs no random access; the files
//! are decoded in order, each once.

use std::collections::{HashMap, HashSet};

use aqr_formula::{Formula, Node};
use aqr_png::similar::Signature;

use crate::{Cx, Format, Made, Nest, Options};

/// A set: its formula (of a root of its outputs, one after another) and the contents it takes
/// the place of, in the order of the outputs.
pub(crate) struct Set {
    pub f: Formula,
    pub members: Vec<usize>,
}

/// Files less alike than this have no parent.
const MIN_ALIKE: f64 = 0.25;

/// Where a content's data is and its length, which tell the content while its formula is
/// where it is.
pub(crate) type Key = (usize, usize);

fn key(d: &[u8]) -> Key {
    (d.as_ptr() as usize, d.len())
}

/// A content's slice (its stream, start and length), which tells it in its formula and any copy.
pub(crate) type At = (usize, u64, u64);

fn at(slice: &Node) -> Option<At> {
    let &Node::Slice { stream, start, len } = slice else { return None };
    Some((stream, start, len))
}

/// A set planned: its members in order (parents first), and the parent of each.
type Planned = (Vec<(Key, At)>, Vec<Option<usize>>);

/// The sets of PNG files planned before they are coded, and all the files looked at.
#[derive(Default)]
pub(crate) struct Plan {
    sets: Vec<Planned>,
    seen: HashSet<At>,
}

impl Plan {
    /// The members with a parent, which are not to be coded alone.
    pub fn children(&self) -> impl Iterator<Item = Key> + '_ {
        self.sets.iter().flat_map(|(m, p)| m.iter().zip(p).filter(|(_, p)| p.is_some()).map(|(k, _)| k.0))
    }

    pub fn is_empty(&self) -> bool {
        self.sets.is_empty()
    }
}

/// The plan of the sets of `files` (PNG files, each the whole of a content, with its slice).
pub(crate) fn plan(files: &[(&[u8], At)], threads: usize) -> Plan {
    let mut seen = HashSet::new();
    let files: Vec<(&[u8], At)> = files.iter().copied().filter(|&(_, at)| seen.insert(at)).collect();
    let data: Vec<&[u8]> = files.iter().map(|f| f.0).collect();
    let sets = forests(&data, threads)
        .into_iter()
        .map(|(order, parents)| (order.into_iter().map(|i| (key(files[i].0), files[i].1)).collect(), parents))
        .collect();
    Plan { sets, seen }
}

/// The trees of `files` alike, of those of the same geometry: of each, the files (their
/// indices) in order, parents first, and the parent of each in that order. Only those where
/// any file has a parent.
fn forests(files: &[&[u8]], threads: usize) -> Vec<(Vec<usize>, Vec<Option<usize>>)> {
    if files.len() < 2 {
        return Vec::new();
    }
    let signatures = par_map(files.len(), threads, |i| Signature::of(files[i]));
    let mut keys: Vec<_> = signatures.iter().flatten().map(Signature::key).collect();
    keys.sort_unstable();
    keys.dedup();
    let mut out = Vec::new();
    for key in keys {
        let group: Vec<usize> =
            (0..files.len()).filter(|&i| signatures[i].as_ref().is_some_and(|s| s.key() == key)).collect();
        if group.len() < 2 {
            continue;
        }
        let sig = |i: usize| signatures[group[i]].as_ref().unwrap();
        let n = group.len();
        let rows = par_map(n, threads, |i| (0..n).map(|j| if i == j { 1.0 } else { sig(i).alike(sig(j)) }).collect());
        let (order, parents) = tree(&rows);
        if parents.iter().any(Option::is_some) {
            out.push((order.into_iter().map(|i| group[i]).collect(), parents));
        }
    }
    out
}

/// The bytes of the records of `f`.
fn records(f: &Formula) -> usize {
    f.streams.iter().filter(|s| s.class == "png/records").map(|s| s.data.len()).sum()
}

/// The sets of the contents `children` of `f` (each its slice and its formula), a formula of
/// data `depth` formats deep, whose formulas they take out of `children`: those of `plan`
/// (whose members with a parent have no formula), and those of the other PNG files coded
/// alone whose set codes smaller.
pub(crate) fn sets(
    f: &Formula,
    children: &mut [(Node, Option<Made>)],
    plan: &Plan,
    opt: &Options,
    depth: u32,
    cx: &Cx,
) -> Vec<Set> {
    let data = |slice: &Node| -> Option<&[u8]> {
        let &Node::Slice { stream, start, len } = slice else { return None };
        f.streams.get(stream)?.data.get(start as usize..)?.get(..len as usize)
    };
    // whether a content's formula is that of a PNG file, all of it
    let png = |slice: &Node, c: &Made| matches!(&c.f.root, Node::Apply(a) if a.module == aqr_png::formula::MODULE && a.len == slice.len());
    let mut out = Vec::new();
    if !plan.is_empty() {
        let mut index = HashMap::new();
        for (i, (slice, _)) in children.iter().enumerate() {
            if let Some(a) = at(slice) {
                index.entry(a).or_insert(i);
            }
        }
        for (keys, parents) in &plan.sets {
            let members: Option<Vec<usize>> = keys.iter().map(|k| index.get(&k.1).copied()).collect();
            let files: Option<Vec<&[u8]>> = members.iter().flatten().map(|&m| data(&children[m].0)).collect();
            let (Some(members), Some(files)) = (members, files) else {
                // (not expected) as contents of their own, those with a parent made now
                for (k, p) in keys.iter().zip(parents) {
                    if let (Some(&m), Some(_)) = (index.get(&k.1), p)
                        && children[m].1.is_none()
                        && let Some(d) = data(&children[m].0)
                    {
                        children[m].1 = crate::made(d, opt, depth + 1, Nest::As(Format::Png), cx)
                            .filter(|c| !c.f.applies().is_empty());
                    }
                }
                continue;
            };
            // the formulas alone that there are (of the members without a parent), taken out
            let alone: Vec<Option<Formula>> = members
                .iter()
                .map(|&m| match &children[m] {
                    (slice, Some(c)) if png(slice, c) => children[m].1.take().map(|c| c.f),
                    _ => None,
                })
                .collect();
            match aqr_png::formula::encode_set(&files, parents, &alone, opt.threads) {
                Some(set) => out.push(Set { f: set, members }),
                None => {
                    // as contents of their own, those with a parent made now
                    for (i, (&m, a)) in members.iter().zip(alone).enumerate() {
                        if let Some(a) = a {
                            children[m].1 = Some(Made::from(a));
                        } else if parents[i].is_some() {
                            children[m].1 = crate::made(files[i], opt, depth + 1, Nest::As(Format::Png), cx)
                                .filter(|c| !c.f.applies().is_empty());
                        }
                    }
                }
            }
        }
    }
    // the other PNG files, each all of its content
    let pngs: Vec<(usize, &[u8])> = children
        .iter()
        .enumerate()
        .filter_map(|(i, (slice, child))| {
            let d = data(slice).filter(|_| at(slice).is_none_or(|a| !plan.seen.contains(&a)))?;
            png(slice, child.as_ref()?).then_some((i, d))
        })
        .collect();
    let files: Vec<&[u8]> = pngs.iter().map(|p| p.1).collect();
    for (order, parents) in forests(&files, opt.threads) {
        let files: Vec<&[u8]> = order.iter().map(|&i| pngs[i].1).collect();
        let members: Vec<usize> = order.iter().map(|&i| pngs[i].0).collect();
        // the formulas alone, taken out (and put back if the set is not taken)
        let formulas: Vec<Option<Formula>> = members.iter().map(|&m| children[m].1.take().map(|c| c.f)).collect();
        let alone: usize = formulas.iter().flatten().map(records).sum();
        let set = aqr_png::formula::encode_set(&files, &parents, &formulas, opt.threads);
        let Some(set) = set.filter(|s| records(s) < alone) else {
            for (&m, f) in members.iter().zip(formulas) {
                children[m].1 = f.map(Made::from);
            }
            continue;
        };
        out.push(Set { f: set, members });
    }
    out
}

/// A maximum spanning forest of `alike` (each row a member's likeness to every member): from
/// the member most like the others, each joined to the one it is most like, while that is
/// [`MIN_ALIKE`] at least; else a tree of its own starts. The members in the order of a
/// depth-first walk (parents first), and the parent of each in that order.
fn tree(alike: &[Vec<f64>]) -> (Vec<usize>, Vec<Option<usize>>) {
    let n = alike.len();
    let mut parent = vec![None; n];
    let mut done = vec![false; n];
    let mut best: Vec<(f64, usize)> = vec![(f64::NEG_INFINITY, 0); n];
    let mut roots = Vec::new();
    for _ in 0..n {
        let next = (0..n).filter(|&k| !done[k]).max_by(|&a, &b| best[a].0.total_cmp(&best[b].0)).unwrap();
        let k = if best[next].0 >= MIN_ALIKE {
            parent[next] = Some(best[next].1);
            next
        } else {
            // a new tree, from the one most like the rest
            let r = (0..n)
                .filter(|&k| !done[k])
                .max_by(|&a, &b| alike[a].iter().sum::<f64>().total_cmp(&alike[b].iter().sum()))
                .unwrap();
            roots.push(r);
            r
        };
        done[k] = true;
        for o in 0..n {
            if !done[o] && alike[k][o] > best[o].0 {
                best[o] = (alike[k][o], k);
            }
        }
    }
    let mut order = Vec::with_capacity(n);
    let mut stack: Vec<usize> = roots.into_iter().rev().collect();
    while let Some(k) = stack.pop() {
        order.push(k);
        stack.extend((0..n).rev().filter(|&c| parent[c] == Some(k)));
    }
    let at: Vec<usize> = {
        let mut at = vec![0; n];
        order.iter().enumerate().for_each(|(i, &k)| at[k] = i);
        at
    };
    let parents = order.iter().map(|&k| parent[k].map(|p| at[p])).collect();
    (order, parents)
}

/// `f(0..n)` on up to `threads` threads, in order.
pub(crate) fn par_map<R: Send>(n: usize, threads: usize, f: impl Fn(usize) -> R + Sync) -> Vec<R> {
    let next = std::sync::atomic::AtomicUsize::new(0);
    let out: Vec<std::sync::Mutex<Option<R>>> = (0..n).map(|_| std::sync::Mutex::new(None)).collect();
    let work = || {
        loop {
            let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if i >= n {
                break;
            }
            *out[i].lock().unwrap() = Some(f(i));
        }
    };
    std::thread::scope(|s| {
        for _ in 1..threads.min(n) {
            s.spawn(work);
        }
        work();
    });
    out.into_iter().map(|m| m.into_inner().unwrap().unwrap()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Trees in depth-first order, parents first; members less alike than [`MIN_ALIKE`] start
    /// trees of their own.
    #[test]
    fn forest() {
        // 0 and 2 alike, 1 like 2, 3 like none
        let alike = vec![
            vec![1.0, 0.3, 0.9, 0.0],
            vec![0.3, 1.0, 0.5, 0.1],
            vec![0.9, 0.5, 1.0, 0.0],
            vec![0.0, 0.1, 0.0, 1.0],
        ];
        let (order, parents) = tree(&alike);
        assert_eq!(order, [2, 0, 1, 3]);
        assert_eq!(parents, [None, Some(0), Some(0), None]);
    }
}
