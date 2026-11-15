//! The alternatives of a batch's formula chosen (`Node::Either`, docs/cross-file-design.md 4.3):
//! where a formula lost to its data as it is by little ([`crate::Verdict::Close`]), both were
//! kept; now that the batch is made, each is chosen by what its bytes archive in after the rest
//! of the batch of their classes, as the archive puts a class's streams one after another: a
//! formula of a version of a program, alike to another version's formula elsewhere in the batch,
//! comes to less after it than the data as it is does after the other's data. The others'
//! formulas count as made (siblings take the same way), those within it as chosen already
//! (innermost first).

use std::collections::HashMap;

use aqr_formula::aqr::Encoder;
use aqr_formula::{Either, Formula, Node};

aqr_common::zopt! {
    /// How the alternatives are chosen: by trial, or all made or all kept (to measure).
    static CHOOSE: Opt<&'static str> =
        Opt::value("defer:choose", "trial", "how a batch's alternatives are chosen").choices(&["trial", "made", "kept"]);
    /// The most bytes of a class's other streams put before an alternative's bytes of the class.
    static PREFIX: Opt<usize> = Opt::size("defer:prefix", 1 << 20, "bytes of the rest of a class put before an alternative's at most");
    /// The most rounds of choosing, each after the others as the round before chose them.
    static ROUNDS: Opt<usize> = Opt::value("defer:rounds", 4, "rounds of choosing the alternatives, each after the last's choices");
}

/// What the alternatives of a batch came to: how many, how many made, and the bytes the
/// formulas archived in alone.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Chosen {
    pub alternatives: usize,
    pub made: usize,
    pub bytes: usize,
}

/// Chooses the alternatives of `f`, a batch's formula (its root's parts its files), and resolves
/// them ([`Formula::resolve_parts`]).
pub(crate) fn choose(f: &mut Formula, enc: &Encoder) -> Chosen {
    let eithers = f.eithers();
    let n = eithers.len();
    if n == 0 {
        return Chosen::default();
    }
    let bytes = eithers.iter().map(|e| e.1.sizes.0).sum();
    let made: Vec<bool> = match CHOOSE.get() {
        "made" => vec![true; n],
        "kept" => vec![false; n],
        _ => {
            let _s = aqr_common::trace::span("choose");
            trial(f, &eithers, enc)
        }
    };
    let count = made.iter().filter(|&&m| m).count();
    f.resolve_parts(&made);
    Chosen { alternatives: n, made: count, bytes }
}

/// Bytes of a stream, by class: the pieces of each.
type ByClass<'a> = HashMap<&'a str, Vec<&'a [u8]>>;

/// Of `eithers` (in [`Formula::eithers`]' order: those within one before it), whether each is
/// made. Each is tried after what the others leave as they are chosen so far, the deepest first
/// (those of a depth on threads): from all kept, again until nothing changes, at most
/// [`ROUNDS`] times. (Counting the others' formulas as made from the start took one alike to a
/// formula that was not kept in the end: WinUHA's Beta split, 137 KB larger.)
fn trial(f: &Formula, eithers: &[(usize, &Either)], enc: &Encoder) -> Vec<bool> {
    let index: HashMap<*const Either, usize> = eithers.iter().enumerate().map(|(i, e)| (e.1 as *const _, i)).collect();
    let deepest = eithers.iter().map(|e| e.0).max().unwrap_or(0);
    let used: Vec<HashMap<usize, Vec<(usize, usize)>>> =
        eithers.iter().map(|e| used_ranges(f, &e.1.kept, &e.1.made)).collect();
    let streams: Vec<(&str, &[u8])> = f.streams.iter().map(|s| (&s.class[..], &s.data[..])).collect();
    let mut made = vec![false; eithers.len()];
    for _ in 0..ROUNDS.get() {
        let live = live_streams(f, &index, &made);
        let mut by_class: HashMap<&str, Vec<usize>> = HashMap::new();
        (0..streams.len()).filter(|&s| live[s]).for_each(|s| by_class.entry(streams[s].0).or_default().push(s));
        let mut next = made.clone();
        for depth in (0..=deepest).rev() {
            let at: Vec<usize> = (0..eithers.len()).filter(|&i| eithers[i].0 == depth).collect();
            // of each, both ways: its pieces of each class, after the prefix of the rest of the
            // class (one for both); tried all at once, each made as it is tried
            let mut jobs: Vec<(&str, usize, [Vec<&[u8]>; 2])> = Vec::new();
            for &i in &at {
                let e = eithers[i].1;
                let mut by: HashMap<&str, [Vec<&[u8]>; 2]> = HashMap::new();
                for (side, node) in [&e.kept, &e.made].into_iter().enumerate() {
                    let mut pieces = ByClass::new();
                    pieces_of(f, node, &index, &next, &mut pieces);
                    for (class, pieces) in pieces {
                        by.entry(class).or_default()[side] = pieces;
                    }
                }
                // (in an order of their own, not the map's)
                let mut by: Vec<_> = by.into_iter().collect();
                by.sort_unstable_by_key(|b| b.0);
                jobs.extend(by.into_iter().map(|(class, sides)| (class, i, sides)));
            }
            let lens = enc
                .len_after_each(jobs.len(), |j| {
                    let (class, i, sides) = &jobs[j];
                    let of = by_class.get(class).map_or(&[][..], |v| &v[..]);
                    (sides.iter().map(|p| p.concat()).collect(), prefix(&streams, of, &used[*i]))
                })
                .expect("zstd");
            let mut cost: HashMap<usize, [usize; 2]> = HashMap::new();
            for ((_, i, _), lens) in jobs.iter().zip(lens) {
                let c = cost.entry(*i).or_default();
                (c[0], c[1]) = (c[0] + lens[0], c[1] + lens[1]);
            }
            for &i in &at {
                let c = cost.get(&i).copied().unwrap_or_default();
                next[i] = c[1] < c[0];
            }
        }
        if next == made {
            break;
        }
        made = next;
    }
    made
}

/// The streams of `f` that a slice takes, the alternatives as `made` tells of them.
fn live_streams(f: &Formula, index: &HashMap<*const Either, usize>, made: &[bool]) -> Vec<bool> {
    fn walk(n: &Node, index: &HashMap<*const Either, usize>, made: &[bool], live: &mut [bool]) {
        match n {
            &Node::Slice { stream, .. } => {
                if let Some(l) = live.get_mut(stream) {
                    *l = true;
                }
            }
            Node::Concat(ns) | Node::Pack(ns) => ns.iter().for_each(|n| walk(n, index, made, live)),
            Node::Apply(a) => a.inputs.iter().for_each(|n| walk(n, index, made, live)),
            Node::Patch(p) => [&p.source, &p.patch].into_iter().for_each(|n| walk(n, index, made, live)),
            Node::Either(e) => {
                let take = index.get(&(&**e as *const Either)).is_some_and(|&i| made[i]);
                walk(if take { &e.made } else { &e.kept }, index, made, live)
            }
            Node::Output { .. } | Node::ModuleBytes(..) | Node::ResourceBytes(_) => {}
        }
    }
    let mut live = vec![false; f.streams.len()];
    f.shared.iter().flat_map(|s| s.inputs()).chain([&f.root]).for_each(|n| walk(n, index, made, &mut live));
    live
}

/// The bytes of node `n` of `f` by the class of their streams, the alternatives within it as
/// `made` tells of them (by their indices in `index`).
fn pieces_of<'a>(
    f: &'a Formula,
    n: &'a Node,
    index: &HashMap<*const Either, usize>,
    made: &[bool],
    out: &mut ByClass<'a>,
) {
    match n {
        &Node::Slice { stream, start, len } => {
            if let Some(s) = f.streams.get(stream)
                && let Some(d) = s.data.get(start as usize..).and_then(|d| d.get(..len as usize))
            {
                out.entry(&s.class).or_default().push(d);
            }
        }
        Node::Concat(ns) | Node::Pack(ns) => ns.iter().for_each(|n| pieces_of(f, n, index, made, out)),
        Node::Apply(a) => a.inputs.iter().for_each(|n| pieces_of(f, n, index, made, out)),
        Node::Patch(p) => [&p.source, &p.patch].into_iter().for_each(|n| pieces_of(f, n, index, made, out)),
        Node::Either(e) => {
            let take = index.get(&(&**e as *const Either)).is_some_and(|&i| made[i]);
            pieces_of(f, if take { &e.made } else { &e.kept }, index, made, out)
        }
        Node::Output { .. } | Node::ModuleBytes(..) | Node::ResourceBytes(_) => {}
    }
}

/// The ranges of each stream that nodes `ns` slice, by stream.
fn used_ranges(f: &Formula, kept: &Node, made: &Node) -> HashMap<usize, Vec<(usize, usize)>> {
    let mut used: HashMap<usize, Vec<(usize, usize)>> = HashMap::new();
    for n in [kept, made] {
        n.visit(&mut |n| {
            if let &Node::Slice { stream, start, len } = n
                && stream < f.streams.len()
            {
                used.entry(stream).or_default().push((start as usize, (start + len) as usize));
            }
        });
    }
    used
}

/// What comes before an alternative's bytes of a class in the archive: the bytes of `of`, the
/// streams of the class that are live (as the others are chosen; of `streams`, the formula's,
/// their class and data), those nearest the alternative's first stream first, the nearest last,
/// but those the alternative takes, up to [`PREFIX`] bytes: of each, only the end that fits.
fn prefix(streams: &[(&str, &[u8])], of: &[usize], used: &HashMap<usize, Vec<(usize, usize)>>) -> Vec<u8> {
    let first = used.keys().copied().min().unwrap_or(0);
    let mut of = of.to_vec();
    of.sort_by_key(|&s| s.abs_diff(first));
    let mut room = PREFIX.get();
    let mut parts: Vec<&[u8]> = Vec::new();
    for s in of {
        if room == 0 {
            break;
        }
        let d = streams[s].1;
        // (the stream but for what the alternative takes of it, from its end)
        let mut cuts = used.get(&s).cloned().unwrap_or_default();
        cuts.sort_unstable();
        let mut rest: Vec<(usize, usize)> = Vec::new();
        let mut at = 0;
        for (a, b) in cuts {
            if a > at {
                rest.push((at, a.min(d.len())));
            }
            at = at.max(b);
        }
        if at < d.len() {
            rest.push((at, d.len()));
        }
        for &(a, b) in rest.iter().rev() {
            if room == 0 {
                break;
            }
            let n = (b - a).min(room);
            room -= n;
            parts.push(&d[b - n..b]);
        }
    }
    // (pieces from the end back, the nearest streams' first: all in order once turned around)
    parts.into_iter().rev().flatten().copied().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use aqr_formula::aqr::Level;

    /// An alternative whose formula is like another file's formula is made; one whose formula is
    /// like nothing, and larger than its data, is kept.
    #[test]
    fn made_where_alike_after_the_rest() {
        let noise = |seed| aqr_common::rng::Rng::new(seed).bytes(200_000);
        let (x, mut y) = (noise(1), noise(1));
        y.iter_mut().step_by(50).for_each(|b| *b ^= 1);
        let mut f = Formula::default();
        let either = |kept, made, len| Node::Either(Box::new(Either { kept, made, len, sizes: (0, 0) }));
        // two files, each its data kept or its formula: the first's alike to the second's, the
        // second's like nothing
        let (k1, k2) = (f.push("raw", noise(2)), f.push("raw", noise(3)));
        let (m1, m2) = (f.push("x", x), f.push("x", noise(4)));
        let other = f.push("x", y);
        let longer = f.push("y", noise(5).repeat(2));
        f.root = Node::Concat(vec![either(k1, m1, 200_000), either(k2, Node::concat([m2, longer]), 200_000), other]);
        // (all kept where told so: as before alternatives were chosen)
        {
            let _kept = CHOOSE.set_here("kept");
            let mut g = f.clone();
            assert_eq!(choose(&mut g, &Encoder::new(Level::MID)).made, 0);
        }
        let chosen = choose(&mut f, &Encoder::new(Level::MID));
        assert_eq!((chosen.alternatives, chosen.made), (2, 1));
        assert!(f.eithers().is_empty());
        // the root's parts stay its parts: the first file's formula, the second's data, the other
        assert_eq!(f.root.len(), 600_000);
        let Node::Concat(parts) = &f.root else { panic!("{f}") };
        assert_eq!(parts.len(), 3);
        let class = |n: &Node| match n {
            &Node::Slice { stream, .. } => f.streams[stream].class.to_string(),
            n => panic!("{n:?}"),
        };
        assert_eq!([class(&parts[0]), class(&parts[1])], ["x", "raw"]);
    }
}
