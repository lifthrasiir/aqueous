//! Sets of files coded against each other: PNG files of the same geometry among the contents of
//! a formula that are alike (variants of a picture, with layers shown or hidden), as one
//! application of aqr-png's `png-set`, each file against its parent along a maximum spanning
//! tree of how alike they are.
//!
//! Within a content, which is decoded whole, the tree's depth costs no random access; the files
//! are decoded in order, each once.

use aqr_formula::{Formula, Node};
use aqr_png::similar::Signature;

use crate::{Made, Options};

/// A set: its formula (of a root of its outputs, one after another) and the contents it takes
/// the place of, in the order of the outputs.
pub(crate) struct Set {
    pub f: Formula,
    pub members: Vec<usize>,
}

/// Files less alike than this have no parent.
const MIN_ALIKE: f64 = 0.25;

/// The sets of the contents `children` of `f` (each its slice and its formula), whose formulas
/// they take out of `children`: those PNG files coded alone whose set codes smaller.
pub(crate) fn sets(f: &Formula, children: &mut [(Node, Option<Made>)], opt: &Options) -> Vec<Set> {
    // PNG files, each all of its content
    let pngs: Vec<(usize, &[u8])> = children
        .iter()
        .enumerate()
        .filter_map(|(i, (slice, child))| {
            let (&Node::Slice { stream, start, len }, Some(Made { f: c, .. })) = (slice, child) else { return None };
            let Node::Apply(a) = &c.root else { return None };
            if a.module != aqr_png::formula::MODULE || a.len != len {
                return None;
            }
            Some((i, f.streams.get(stream)?.data.get(start as usize..)?.get(..len as usize)?))
        })
        .collect();
    if pngs.len() < 2 {
        return Vec::new();
    }
    let signatures = par_map(pngs.len(), opt.threads, |i| Signature::of(pngs[i].1));
    let mut keys: Vec<_> = signatures.iter().flatten().map(Signature::key).collect();
    keys.sort_unstable();
    keys.dedup();
    let mut out = Vec::new();
    for key in keys {
        let group: Vec<usize> =
            (0..pngs.len()).filter(|&i| signatures[i].as_ref().is_some_and(|s| s.key() == key)).collect();
        if group.len() < 2 {
            continue;
        }
        let sig = |i: usize| signatures[group[i]].as_ref().unwrap();
        let n = group.len();
        let rows =
            par_map(n, opt.threads, |i| (0..n).map(|j| if i == j { 1.0 } else { sig(i).alike(sig(j)) }).collect());
        let (order, parents) = tree(&rows);
        if parents.iter().all(Option::is_none) {
            continue;
        }
        let files: Vec<&[u8]> = order.iter().map(|&i| pngs[group[i]].1).collect();
        let Some(set) = aqr_png::formula::encode_set(&files, &parents, opt.threads) else { continue };
        let members: Vec<usize> = order.iter().map(|&i| pngs[group[i]].0).collect();
        let records = |f: &Formula| -> usize {
            f.streams.iter().filter(|s| s.class == "png/records").map(|s| s.data.len()).sum()
        };
        let alone: usize = members.iter().map(|&m| children[m].1.as_ref().map_or(0, |c| records(&c.f))).sum();
        if records(&set) >= alone {
            continue;
        }
        members.iter().for_each(|&m| children[m].1 = None);
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
fn par_map<R: Send>(n: usize, threads: usize, f: impl Fn(usize) -> R + Sync) -> Vec<R> {
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
