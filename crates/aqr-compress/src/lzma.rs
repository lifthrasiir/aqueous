//! `-Z lzma`: streams of a batch compressed by LZMA (XZ Utils' liblzma) in place of the
//! archive's zstd, an experiment on what LZMA would save. Each stream taken is stored as it is,
//! a `.lzma` file, and every slice of it becomes an application of aqr-lzma's `lzma-decode`
//! to it. Only streams that are sliced whole (every slice of them all of them) are taken, as
//! an application's output is not sliced; and only those long enough, as each is compressed
//! alone (the shorter ones are left to the archive, which compresses them together); and none
//! that a sink has written already (`aqr::sink`).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

use aqr_formula::{Formula, Kind, Node};

aqr_common::zopt! {
    /// Whether the streams are compressed by LZMA rather than zstd ([`apply`]).
    pub(crate) static LZMA: Opt<bool> = Opt::flag("lzma", false, "streams compressed by LZMA (liblzma, lzma-decode) rather than zstd");
    /// The shortest stream compressed by LZMA.
    static MIN: Opt<usize> = Opt::size("lzma:store-min", 64 << 10, "shortest stream that -Z lzma compresses by LZMA");
    /// The preset of liblzma.
    static PRESET: Opt<u32> = Opt::value("lzma:store-preset", 9, "liblzma's preset (0-9) of -Z lzma's streams");
    /// Whether the preset is extreme.
    static EXTREME: Opt<bool> = Opt::flag("lzma:store-extreme", false, "liblzma's extreme preset of -Z lzma's streams");
}

/// Bytes of the streams taken and of their `.lzma` files, of the whole run.
static TAKEN: [AtomicU64; 3] = [const { AtomicU64::new(0) }; 3];

/// `data` as a `.lzma` file (LZMA-alone: an end marker, the size unknown), of a dictionary
/// no larger than the data needs.
fn compress(data: &[u8]) -> Vec<u8> {
    use liblzma::stream::{Action, LzmaOptions, PRESET_EXTREME, Status, Stream};
    let preset = PRESET.get() | if EXTREME.get() { PRESET_EXTREME } else { 0 };
    let mut opts = LzmaOptions::new_preset(preset).expect("a preset of liblzma");
    // (the dictionary of the preset, but no larger than the data: the decoder's memory)
    let preset_dict = [256 << 10, 1 << 20, 2 << 20, 4 << 20, 4 << 20, 8 << 20, 8 << 20, 16 << 20, 32 << 20, 64 << 20]
        [PRESET.get().min(9) as usize];
    opts.dict_size((data.len().next_power_of_two() as u32).clamp(4096, preset_dict));
    let mut s = Stream::new_lzma_encoder(&opts).expect("liblzma's encoder");
    let mut out = Vec::with_capacity(data.len() / 3 + 4096);
    loop {
        let at = s.total_in() as usize;
        if out.capacity() - out.len() < 4096 {
            out.reserve(out.capacity() / 2 + 4096);
        }
        if s.process_vec(&data[at..], &mut out, Action::Finish).expect("liblzma") == Status::StreamEnd {
            return out;
        }
    }
}

/// How the slices of a stream take it: all of it each, or in pieces one after another, each
/// once (of one leaf, where that matters), or otherwise.
#[derive(Clone, PartialEq)]
enum Taken {
    None,
    Whole,
    Pieces(Vec<(u64, u64)>, Option<usize>),
    Other,
}

/// Takes the streams of `f` to compress by LZMA where `-Z lzma` is given: its root a file's
/// (`leaves` None), or the outputs of a batch's leaves, `leaves` of them each, one after another.
/// A stream sliced whole is an application of `lzma-decode` wherever it is sliced; one sliced in
/// pieces one after another, each once, its output split into them (of a single leaf, not in an
/// application of several outputs: each is taken in the leaf it is made in). Where the `.lzma`
/// file saves less than 1/32, the stream is left as it is.
pub(crate) fn apply(f: &mut Formula, leaves: Option<&[usize]>, threads: usize) {
    if !LZMA.get() {
        return;
    }
    let _s = aqr_common::trace::span("lzma");
    let mut taken = vec![Taken::None; f.streams.len()];
    let lens: Vec<u64> = f.streams.iter().map(|s| s.data.len() as u64).collect();
    // (`leaf` None: in an application of several outputs)
    let mut look = |n: &Node, leaf: Option<usize>| {
        n.visit(&mut |n| {
            let &Node::Slice { stream, start, len } = n else { return };
            let t = &mut taken[stream];
            let whole = start == 0 && len == lens[stream];
            let placed = leaves.is_none() || leaf.is_some();
            *t = match std::mem::replace(t, Taken::None) {
                Taken::None | Taken::Whole if whole => Taken::Whole,
                Taken::None if placed => Taken::Pieces(vec![(start, len)], leaf),
                Taken::Pieces(mut p, l) if !whole && placed && l == leaf => {
                    p.push((start, len));
                    Taken::Pieces(p, l)
                }
                _ => Taken::Other,
            };
        })
    };
    f.shared.iter().flat_map(|s| s.inputs()).for_each(|n| look(n, None));
    match (&f.root, leaves) {
        (Node::Concat(ns), Some(counts)) => {
            let mut at = 0;
            for (l, &k) in counts.iter().enumerate() {
                ns[at..at + k].iter().for_each(|n| look(n, Some(l)));
                at += k;
            }
        }
        (root, _) => look(root, Some(0)),
    }
    for (s, t) in taken.iter_mut().enumerate() {
        if let Taken::Pieces(p, _) = t {
            p.sort_unstable();
            let mut end = 0;
            let tiles = p.iter().all(|&(a, n)| std::mem::replace(&mut end, a + n) == a && n > 0);
            if !tiles || end != lens[s] {
                *t = Taken::Other;
            }
        }
    }
    let min = MIN.get();
    let chosen: Vec<usize> = (0..f.streams.len())
        .filter(|&s| {
            let st = &f.streams[s];
            matches!(taken[s], Taken::Whole | Taken::Pieces(..))
                && st.kind != Kind::Compressed
                && st.data.len() >= min
                && !st.data.is_lazy()
                && st.data.sunk_in().is_none()
        })
        .collect();
    let made = aqr_common::pool::par_map(chosen.len(), threads, |i| {
        let d = &f.streams[chosen[i]].data;
        let c = compress(d);
        (c.len() < d.len() - d.len() / 32).then_some(c)
    });
    // (of each stream taken: the node of all of it, or the outputs of its pieces, by start)
    let mut whole = vec![None; f.streams.len()];
    let mut pieces: HashMap<(usize, u64), Node> = HashMap::new();
    // (the applications split, pushed once the old slices are replaced: their outputs are
    // known before, of the indices they will have)
    let mut splits = Vec::new();
    for (&s, c) in chosen.iter().zip(made) {
        let Some(c) = c else { continue };
        let len = lens[s];
        TAKEN[0].fetch_add(len, Relaxed);
        TAKEN[1].fetch_add(c.len() as u64, Relaxed);
        TAKEN[2].fetch_add(1, Relaxed);
        let Node::Apply(mut a) = aqr_lzma::formula::decoded(Node::slice(s, 0, c.len() as u64), len) else {
            unreachable!()
        };
        let limit = crate::memory::limit(&a, &[c.len() as u64], &[len], &vec![&c[..]]);
        (a.memory_limit, a.task_limits) = (limit.instance, limit.tasks);
        let st = &mut f.streams[s];
        st.data = c.into();
        st.kind = Kind::Compressed;
        match &taken[s] {
            Taken::Pieces(p, _) => {
                let shared = f.shared.len() + splits.len();
                for (index, &(start, len)) in p.iter().enumerate() {
                    pieces.insert((s, start), Node::Output { shared, index, len });
                }
                splits.push((*a, p.iter().map(|x| x.1).collect::<Vec<u64>>()));
            }
            _ => whole[s] = Some(Node::Apply(a)),
        }
    }
    if TAKEN[2].load(Relaxed) > 0 {
        let [a, b, n] = [0, 1, 2].map(|k| TAKEN[k].load(Relaxed));
        eprintln!("aqr-compress: -Z lzma: {n} streams so far, {a} -> {b} bytes");
    }
    f.map_slices(&mut |stream, start, len| {
        if let Some(w) = &whole[stream] {
            return w.clone();
        }
        match pieces.remove(&(stream, start)) {
            Some(o) => o,
            None => Node::Slice { stream, start, len },
        }
    });
    debug_assert!(pieces.is_empty());
    let before = f.shared.len();
    for (a, sizes) in splits {
        f.split(a, sizes);
    }
    // (the applications made first: those before may take their outputs)
    let made = f.shared.len();
    if made > before {
        let order: Vec<usize> = (before..made).chain(0..before).collect();
        f.reorder_shared(&order);
    }
}

#[cfg(test)]
mod tests {
    /// liblzma's `.lzma` files decode by `lzma-decode` to their data, and not to another length.
    #[test]
    fn decodes() {
        let mut d = Vec::new();
        let mut x = 1u32;
        for i in 0..300_000u32 {
            x = x.wrapping_mul(1_103_515_245).wrapping_add(12345);
            d.push(if i % 7 < 3 { (x >> 24) as u8 } else { b"lorem ipsum "[(i % 12) as usize] });
        }
        for n in [0, 1, 100, d.len()] {
            let c = super::compress(&d[..n]);
            let out = aqr_lzma::module::decode_alone(&[], &[&c], n as u64).unwrap();
            assert!(out == d[..n], "{n}");
            assert!(aqr_lzma::module::decode_alone(&[], &[&c], n as u64 + 1).is_err(), "{n}");
        }
    }
}
