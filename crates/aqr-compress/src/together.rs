//! `-Z together:always`: whole batches compressed solid, for archives read whole (a
//! self-extracting one, say), where random access does not matter. The streams of a batch, of
//! all classes, one after another in one stream (or several, of zstd's largest window each),
//! compressed as one zstd block at `zstd:together-level` from `zstd:dense-from` on. A batch
//! (`-B`) still bounds what is together, and the memory that takes. With `-Z lzma`, LZMA's
//! groups first ([`crate::lzma`]).

use aqr_archive::TOGETHER;
use aqr_formula::{Formula, Kind, Node};

aqr_common::zopt! {
    /// Whether the streams of a batch are put together, all of its classes ([`apply`]).
    pub(crate) static ALWAYS: Opt<bool> = Opt::flag("together:always", false, "the streams of a batch, of all classes, in one stream (whole batches solid; -Z lzma implies it)");
}

/// Of the streams of `f`: every slice where the formula takes it, in the order that
/// `map_slices` visits them; whether each stream may be taken (not [`Kind::Compressed`], made,
/// not sunk, no slice past its end); and the rank of its class and kind, in the order of their
/// first streams.
#[allow(clippy::type_complexity)]
pub(crate) fn survey(f: &mut Formula) -> (Vec<(usize, u64, u64)>, Vec<bool>, Vec<usize>) {
    let n = f.streams.len();
    let lens: Vec<u64> = f.streams.iter().map(|s| s.data.len() as u64).collect();
    let mut slices: Vec<(usize, u64, u64)> = Vec::new();
    let mut past = vec![false; n];
    f.map_slices(&mut |stream, start, len| {
        if stream < n {
            past[stream] |= start.checked_add(len).is_none_or(|e| e > lens[stream]);
        }
        slices.push((stream, start, len));
        Node::Slice { stream, start, len }
    });
    let taken: Vec<bool> = (0..n)
        .map(|s| {
            let st = &f.streams[s];
            !past[s] && st.kind != Kind::Compressed && !st.data.is_lazy() && st.data.sunk_in().is_none()
        })
        .collect();
    let mut rank: Vec<usize> = vec![0; n];
    let mut classes: Vec<(&str, Kind)> = Vec::new();
    for (s, st) in f.streams.iter().enumerate() {
        let c = (&*st.class, st.kind);
        rank[s] = classes.iter().position(|&k| k == c).unwrap_or_else(|| {
            classes.push(c);
            classes.len() - 1
        });
    }
    (slices, taken, rank)
}

/// The streams of `f` that [`survey`] takes put one after another (of a class together, in the
/// order of their classes' first streams) in streams of zstd's largest window at most, of the
/// class [`TOGETHER`], where `together:always` (or `-Z lzma`) is given: what the archive does of
/// a class's streams ([`Formula::merge_classes_within`]), of all of them. Their slices stay
/// slices, so what files share is stored once. A stream longer than that is left as it is.
pub(crate) fn apply(f: &mut Formula) {
    if !(ALWAYS.get() || crate::lzma::LZMA.get()) {
        return;
    }
    let _s = aqr_common::trace::span("together");
    let n = f.streams.len();
    let (_, taken, rank) = survey(f);
    let mut order: Vec<usize> = (0..n).filter(|&s| taken[s] && !f.streams[s].data.is_empty()).collect();
    order.sort_by_key(|&s| (rank[s], s));
    let max = 1u64 << aqr_archive::WINDOW_LOG_MAX;
    // (of each stream taken, the stream it goes to, as it is to be pushed, and where in it)
    let mut to: Vec<Option<(usize, u64)>> = vec![None; n];
    let mut groups: Vec<Vec<usize>> = Vec::new();
    let mut bytes = 0;
    for &s in &order {
        let l = f.streams[s].data.len() as u64;
        if l > max {
            continue;
        }
        match groups.last_mut() {
            Some(g) if bytes + l <= max => {
                g.push(s);
                bytes += l;
            }
            _ => {
                groups.push(vec![s]);
                bytes = l;
            }
        }
    }
    for g in groups.iter().filter(|g| g.len() > 1) {
        let total: usize = g.iter().map(|&s| f.streams[s].data.len()).sum();
        let mut d = Vec::with_capacity(total);
        let at = f.streams.len();
        for &s in g {
            to[s] = Some((at, d.len() as u64));
            d.extend_from_slice(&f.streams[s].data);
        }
        f.push(TOGETHER, d);
    }
    f.map_slices(&mut |stream, start, len| match to.get(stream).copied().flatten() {
        Some((s, at)) => Node::Slice { stream: s, start: at + start, len },
        None => Node::Slice { stream, start, len },
    });
    f.compact();
}
