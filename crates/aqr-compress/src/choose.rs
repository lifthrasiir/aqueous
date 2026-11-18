//! The alternatives of a batch's formula chosen (`Node::Either`, docs/cross-file-design.md 4.3):
//! where a formula lost to its data as it is by little ([`crate::Verdict::Close`]), both were
//! kept; now that the batch is made, each is chosen by what its bytes archive in after the rest
//! of the batch of their classes, as the archive puts a class's streams one after another: a
//! formula of a version of a program, alike to another version's formula elsewhere in the batch,
//! comes to less after it than the data as it is does after the other's data. The others'
//! formulas count as made (siblings take the same way), those within it as chosen already
//! (innermost first).
//!
//! A formula whose alternatives are all chosen is [`Settled`], made only here: what is declared,
//! sealed and archived takes one, so that no formula holding alternatives comes to them.

use std::collections::HashMap;
use std::ops::{Deref, DerefMut};

use aqr_archive::Encoder;
use aqr_formula::{Apply, Either, Formula, Module, Node, Of, Resource};

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

/// A formula holding no alternatives ([`Node::Either`]): chosen ([`choose`], [`by_blocks`]), or
/// their data kept ([`kept`]). Its memory is declared ([`crate::memory::declare`]) and it is
/// sealed ([`crate::seal::Sink`]) and archived as this alone. What changes it (through
/// [`DerefMut`]) puts no alternative in it.
#[derive(Debug, Clone, Default)]
pub(crate) struct Settled(Formula);

impl Settled {
    pub fn into_inner(self) -> Formula {
        self.0
    }
}

impl Deref for Settled {
    type Target = Formula;

    fn deref(&self) -> &Formula {
        &self.0
    }
}

impl DerefMut for Settled {
    fn deref_mut(&mut self) -> &mut Formula {
        &mut self.0
    }
}

/// `f` with the data of its alternatives kept, as they were compared: where nothing chooses
/// (a content sealed as it is made, a formula of a file that is not alone).
pub(crate) fn kept(mut f: Formula) -> Settled {
    let n = f.eithers().len();
    if n > 0 {
        f.resolve(&vec![false; n]);
    }
    Settled(f)
}

/// What `go` gives of formula `f` [`kept`] in place: of a formula that a recompressor hands
/// through `&mut` (aqr-tensors' parts).
pub(crate) fn kept_in<R>(f: &mut Formula, go: impl FnOnce(&mut Settled) -> R) -> R {
    let mut s = kept(std::mem::take(f));
    let r = go(&mut s);
    *f = s.0;
    r
}

/// Chooses the alternatives of `f`, a batch's formula (its root's parts its files), and resolves
/// them ([`Formula::resolve_parts`]).
pub(crate) fn choose(mut f: Formula, enc: &Encoder) -> (Settled, Chosen) {
    let chosen = choose_in(&mut f, enc);
    (Settled(f), chosen)
}

/// [`choose`] in place.
fn choose_in(f: &mut Formula, enc: &Encoder) -> Chosen {
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

/// A block that an archive holds once however many applications take it.
#[derive(Clone, PartialEq, Eq, Hash)]
enum Block {
    Module(Module),
    Resource(Resource),
}

/// Of a file archived alone ([`crate::recompress_at`]), chooses the alternatives of `f`, those
/// that lost to their data only by the module and resource blocks they take among them
/// ([`crate::Verdict::Close`], sized without those), and resolves them ([`Formula::resolve`]):
/// made where the blocks they take are paid for, by what all the alternatives made that take a
/// block gain (each gain shared among the blocks it takes by their bytes); one taking a block
/// that is not paid for is kept, and so is any other taking it. The blocks the rest of the formula
/// takes are paid for already. An alternative of a scan whose files were all kept beside their
/// data (sized `(0, len)`, [`crate::scanned`]) gains nothing itself: it is made where one within
/// it is.
pub(crate) fn by_blocks(mut f: Formula, enc: &Encoder) -> (Settled, Chosen) {
    let chosen = by_blocks_in(&mut f, enc);
    (Settled(f), chosen)
}

/// [`by_blocks`] in place.
fn by_blocks_in(f: &mut Formula, enc: &Encoder) -> Chosen {
    let eithers = f.eithers();
    let n = eithers.len();
    if n == 0 {
        return Chosen::default();
    }
    let index: HashMap<*const Either, usize> = eithers.iter().enumerate().map(|(i, e)| (e.1 as *const _, i)).collect();
    let parent = parents(f, &index);
    let mut cost: HashMap<Block, usize> = HashMap::new();
    let mut paid: Vec<Block> = Vec::new();
    blocks_of(f, &f.root, &mut paid);
    let own: Vec<Vec<Block>> = eithers
        .iter()
        .map(|(_, e)| {
            let mut own = Vec::new();
            blocks_of(f, &e.made, &mut own);
            own.retain(|b| !paid.contains(b));
            for b in &own {
                if !cost.contains_key(b) {
                    let c = match b {
                        Block::Module(m) => enc.module_once_len(*m),
                        Block::Resource(r) => enc.resource_once_len(r),
                    };
                    cost.insert(b.clone(), c.expect("modules and resources of the formula"));
                }
            }
            own.retain(|b| cost[b] > 0);
            own
        })
        .collect();
    let scans: Vec<bool> = eithers.iter().map(|(_, e)| e.sizes.0 == 0 && e.sizes.1 as u64 == e.len).collect();
    let gain: Vec<f64> = eithers.iter().map(|(_, e)| e.sizes.1 as f64 - e.sizes.0 as f64).collect();
    let mut made: Vec<bool> = (0..n).map(|i| !scans[i] && gain[i] > 0.0).collect();
    let mut unpaid: Vec<Block> = Vec::new();
    loop {
        // (an alternative counts where what holds it is chosen: parents after their children)
        let mut live = vec![false; n];
        for i in (0..n).rev() {
            live[i] = match parent[i] {
                None => true,
                Some((p, true)) => live[p] && (made[p] || scans[p]),
                Some((p, false)) => live[p] && !made[p],
            };
        }
        let mut paying: HashMap<&Block, f64> = HashMap::new();
        for i in (0..n).filter(|&i| live[i] && made[i]) {
            let total: usize = own[i].iter().map(|b| cost[b]).sum();
            for b in &own[i] {
                *paying.entry(b).or_default() += gain[i] * cost[b] as f64 / total as f64;
            }
        }
        let short: Vec<Block> =
            paying.into_iter().filter(|&(b, p)| p < cost[b] as f64).map(|(b, _)| b.clone()).collect();
        if short.is_empty() {
            break;
        }
        unpaid.extend(short);
        for i in 0..n {
            made[i] &= !own[i].iter().any(|b| unpaid.contains(b));
        }
    }
    // (a scan's alternative made where one within it is: those within come first)
    for i in 0..n {
        if made[i]
            && let Some((p, true)) = parent[i]
            && scans[p]
        {
            made[p] = true;
        }
    }
    // together, as the archive has them: what each gained apart may be what the data kept
    // shares with the rest of it (copies of a file), which none alone tells
    if made.iter().any(|&m| m) && !pays_together(f, &index, &parent, &made, &own, &cost, enc) {
        made.fill(false);
    }
    let chosen = Chosen {
        alternatives: n,
        made: made.iter().filter(|&&m| m).count(),
        bytes: eithers.iter().map(|e| e.1.sizes.0).sum(),
    };
    f.resolve(&made);
    chosen
}

/// Whether the alternatives of `f` that `made` tells of ([`by_blocks`]: of the blocks they take,
/// `own`, of bytes `cost`) archive in less made than kept, all together: the outermost of those
/// chosen, each side's bytes of a class one after another, and the blocks.
fn pays_together(
    f: &Formula,
    index: &HashMap<*const Either, usize>,
    parent: &[Option<(usize, bool)>],
    made: &[bool],
    own: &[Vec<Block>],
    cost: &HashMap<Block, usize>,
    enc: &Encoder,
) -> bool {
    let eithers = f.eithers();
    let n = eithers.len();
    // (those in what is chosen: parents after their children)
    let mut live = vec![false; n];
    for i in (0..n).rev() {
        live[i] = parent[i].is_none_or(|(p, made_side)| live[p] && made[p] == made_side);
    }
    let (kept, mut sides): (Vec<bool>, HashMap<&str, Sides>) = (vec![false; n], HashMap::new());
    let mut blocks: Vec<&Block> = Vec::new();
    for i in (0..n).filter(|&i| live[i] && made[i]) {
        own[i].iter().for_each(|b| {
            if !blocks.contains(&b) {
                blocks.push(b)
            }
        });
        // (the outermost: those within it are of its sides)
        if parent[i].is_some_and(|(p, _)| made[p]) {
            continue;
        }
        let e = eithers[i].1;
        for (side, (node, of)) in [(&e.kept, &kept[..]), (&e.made, made)].into_iter().enumerate() {
            let mut pieces = ByClass::new();
            pieces_of(f, node, index, of, &mut pieces);
            for (class, pieces) in pieces {
                sides.entry(class).or_default()[side].extend(pieces);
            }
        }
    }
    let mut sides: Vec<_> = sides.into_iter().collect();
    sides.sort_unstable_by_key(|s| s.0);
    let lens = enc
        .len_after_each(sides.len(), |j| (sides[j].1.iter().map(|p| p.concat()).collect(), Vec::new()))
        .expect("zstd");
    let (kept, made) = lens.iter().fold((0, 0), |(k, m), l| (k + l[0], m + l[1]));
    made + blocks.iter().map(|b| cost[*b]).sum::<usize>() < kept
}

/// Of each alternative of `f` (by its index in `index`), the one it is within, if any, and
/// whether on the side of the formula made.
fn parents(f: &Formula, index: &HashMap<*const Either, usize>) -> Vec<Option<(usize, bool)>> {
    fn walk(
        n: &Node,
        at: Option<(usize, bool)>,
        index: &HashMap<*const Either, usize>,
        out: &mut [Option<(usize, bool)>],
    ) {
        match n {
            Node::Either(e) => {
                let i = index[&(&**e as *const Either)];
                out[i] = at;
                walk(&e.kept, Some((i, false)), index, out);
                walk(&e.made, Some((i, true)), index, out);
            }
            Node::Concat(ns) | Node::Pack(ns) => ns.iter().for_each(|n| walk(n, at, index, out)),
            Node::Apply(a) => a.inputs.iter().for_each(|n| walk(n, at, index, out)),
            Node::Patch(p) => [&p.source, &p.patch].into_iter().for_each(|n| walk(n, at, index, out)),
            Node::Slice { .. } | Node::Output { .. } | Node::ModuleBytes(..) | Node::ResourceBytes(_) => {}
        }
    }
    let mut out = vec![None; index.len()];
    f.shared.iter().flat_map(|s| s.inputs()).chain([&f.root]).for_each(|n| walk(n, None, index, &mut out));
    out
}

/// The blocks that node `n` of `f` takes, with the applications of several outputs it takes
/// outputs of, into `out` (each once): of the alternatives within it, those of the data kept.
fn blocks_of(f: &Formula, n: &Node, out: &mut Vec<Block>) {
    fn walk(f: &Formula, n: &Node, seen: &mut [bool], out: &mut Vec<Block>) {
        let apply = |a: &Apply, out: &mut Vec<Block>| {
            let blocks = [Block::Module(a.module)].into_iter().chain(a.resources.iter().cloned().map(Block::Resource));
            for b in blocks {
                if !out.contains(&b) {
                    out.push(b);
                }
            }
        };
        match n {
            Node::Apply(a) => {
                apply(a, out);
                a.inputs.iter().for_each(|n| walk(f, n, seen, out));
            }
            &Node::Output { shared, .. } => {
                if let Some(s) = f.shared.get(shared)
                    && !std::mem::replace(&mut seen[shared], true)
                {
                    if let Of::Apply(a) = &s.of {
                        apply(a, out);
                    }
                    s.inputs().iter().for_each(|n| walk(f, n, seen, out));
                }
            }
            Node::Either(e) => walk(f, &e.kept, seen, out),
            Node::Concat(ns) | Node::Pack(ns) => ns.iter().for_each(|n| walk(f, n, seen, out)),
            Node::Patch(p) => [&p.source, &p.patch].into_iter().for_each(|n| walk(f, n, seen, out)),
            Node::Slice { .. } | Node::ModuleBytes(..) | Node::ResourceBytes(_) => {}
        }
    }
    walk(f, n, &mut vec![false; f.shared.len()], out);
}

/// Bytes of a stream, by class: the pieces of each.
type ByClass<'a> = HashMap<&'a str, Vec<&'a [u8]>>;

/// The pieces of an Either's two sides (kept, made) of a class.
type Sides<'a> = [Vec<&'a [u8]>; 2];

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
            let mut jobs: Vec<(&str, usize, Sides)> = Vec::new();
            for &i in &at {
                let e = eithers[i].1;
                let mut by: HashMap<&str, Sides> = HashMap::new();
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
    use aqr_archive::Level;

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
            assert_eq!(choose_in(&mut g, &Encoder::new(Level::MID)).made, 0);
        }
        let chosen = choose_in(&mut f, &Encoder::new(Level::MID));
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

    /// Of a file alone, alternatives that each gain less than the module block they take are
    /// made where together they pay for it, or where the rest of the formula takes it already;
    /// one alone is kept. One of a scan's (sized `(0, len)`) is made where one within it is.
    #[test]
    fn made_where_their_module_is_paid_for() {
        let enc = crate::encoder(&crate::Options::default());
        let module = aqr_flate::formula::MODULE;
        let block = enc.module_once_len(module).expect("its Wasm module");
        assert!(block > 1000, "{block}");
        // (an alternative of the data kept, noise, or an application of the module, gaining
        // `gain`)
        let seed = std::cell::Cell::new(0);
        let either = |f: &mut Formula, gain: usize| {
            seed.set(seed.get() + 1);
            let len = 50 + gain;
            let kept = f.push("raw", aqr_common::rng::Rng::new(seed.get()).bytes(len));
            let made = Node::apply(module, Vec::new(), vec![f.push("x", vec![2; 50])], len as u64);
            Node::Either(Box::new(Either { kept, made, len: len as u64, sizes: (50, len) }))
        };
        let made = |n: usize, gain: usize, paid: bool| {
            let mut f = Formula::default();
            let mut parts: Vec<Node> = (0..n).map(|_| either(&mut f, gain)).collect();
            if paid {
                parts.push(Node::apply(module, Vec::new(), vec![f.push("x", vec![3; 50])], 100));
            }
            f.root = Node::Concat(parts);
            let chosen = by_blocks_in(&mut f, &enc);
            assert!(f.eithers().is_empty());
            chosen.made
        };
        let part = block * 6 / 10;
        assert_eq!(made(1, part, false), 0);
        assert_eq!(made(2, part, false), 2);
        assert_eq!(made(1, part, true), 1);
        // (one losing even without the block is kept)
        assert_eq!(made(1, 0, true), 0);
        // (nor where what they gained apart is what their data kept has in common: copies)
        let mut f = Formula::default();
        let copies: Vec<Node> = (0..2).map(|_| either(&mut f, part)).collect();
        f.streams[2].data = f.streams[0].data.clone();
        f.root = Node::Concat(copies);
        assert_eq!(by_blocks_in(&mut f, &enc).made, 0);

        // a scan's alternative, its files' alternatives within the formula made
        let mut f = Formula::default();
        let inner: Vec<Node> = (0..2).map(|_| either(&mut f, part)).collect();
        let len = 2 * (50 + part);
        let kept = f.push("raw", aqr_common::rng::Rng::new(99).bytes(len));
        let scan = Either { kept, made: Node::Concat(inner), len: len as u64, sizes: (0, len) };
        f.root = Node::Either(Box::new(scan));
        assert_eq!(by_blocks_in(&mut f, &enc).made, 3);
        assert_eq!(f.applies().len(), 2);
    }
}
