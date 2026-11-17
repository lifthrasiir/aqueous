//! `-Z lzma`: the streams of a batch compressed by LZMA (XZ Utils' liblzma) in place of the
//! archive's zstd, an experiment on what LZMA would save. The slices of the streams taken are
//! put one after another (of a class together, each stream's in order), the bytes of each slice
//! once for each time it is taken, and compressed together, in groups of `lzma:group` bytes at
//! most, each a `.lzma` file stored as it is: an application of aqr-lzma's `lzma-decode`, its
//! output split into the slices. The formula takes each output once, so that what several
//! files share (the slices of `Formula::share_slices`) is in the group as often as it is
//! taken, which LZMA's window codes as matches (as NOSSO does, with no dedup of its own). The
//! streams left are those that [`crate::together::survey`] does not take, and those of groups
//! that LZMA saves little of; `together:always`, which `-Z lzma` implies, puts the latter in one
//! stream for zstd.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

use aqr_formula::{Formula, Kind, Node};

aqr_common::zopt! {
    /// Whether the streams are compressed by LZMA rather than zstd ([`apply`]), all of a batch
    /// together as `together:always` puts them.
    pub(crate) static LZMA: Opt<bool> = Opt::flag("lzma", false, "streams compressed by LZMA (liblzma, lzma-decode) rather than zstd, a batch's together (implies together:always)");
    /// The most bytes of a group, compressed as one `.lzma` file.
    static GROUP: Opt<usize> = Opt::size("lzma:group", 128 << 20, "most bytes of -Z lzma's groups, each compressed as one .lzma file");
    /// The largest dictionary.
    static DICT: Opt<usize> = Opt::size("lzma:store-dict", 128 << 20, "largest dictionary of -Z lzma's .lzma files (the encoder takes about 11 times it)");
    /// The preset of liblzma.
    static PRESET: Opt<u32> = Opt::value("lzma:store-preset", 9, "liblzma's preset (0-9) of -Z lzma's streams");
    /// Whether the preset is extreme.
    static EXTREME: Opt<bool> = Opt::flag("lzma:store-extreme", false, "liblzma's extreme preset of -Z lzma's streams");
}

/// Bytes of the groups and of their `.lzma` files, and the groups, of the whole run.
static TAKEN: [AtomicU64; 3] = [const { AtomicU64::new(0) }; 3];

/// `data` as a `.lzma` file (LZMA-alone: an end marker, the size unknown), of a dictionary
/// no larger than the data needs (nor `lzma:store-dict`).
fn compress(data: &[u8]) -> Vec<u8> {
    use liblzma::stream::{Action, LzmaOptions, PRESET_EXTREME, Status, Stream};
    let preset = PRESET.get() | if EXTREME.get() { PRESET_EXTREME } else { 0 };
    let mut opts = LzmaOptions::new_preset(preset).expect("a preset of liblzma");
    opts.dict_size(data.len().next_power_of_two().min(DICT.get()).clamp(4096, 1 << 30) as u32);
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

/// Takes the slices of the streams of `f` into groups compressed by LZMA where `-Z lzma` is
/// given ([the module's](self)), each an application of several outputs, whose outputs the
/// slices become. Returns whether any is made: the formula's parts taking them must then be of
/// one leaf, as each output is taken in the leaf it is made in. A group whose `.lzma` file saves
/// less than 1/32 is left as it was.
pub(crate) fn apply(f: &mut Formula, threads: usize) -> bool {
    if !LZMA.get() {
        return false;
    }
    let _s = aqr_common::trace::span("lzma");
    let n = f.streams.len();
    // (the order of the bytes: the classes in the order of their first streams, each stream's
    // slices by where they start, the longest first)
    let (slices, taken, rank) = crate::together::survey(f);
    let mut order: Vec<usize> =
        (0..slices.len()).filter(|&k| slices[k].0 < n && taken[slices[k].0] && slices[k].2 > 0).collect();
    order.sort_by_key(|&k| {
        let (s, a, l) = slices[k];
        (rank[s], s, a, std::cmp::Reverse(l))
    });
    if order.is_empty() {
        return false;
    }
    // (the groups, of consecutive slices in `order`)
    let solid = GROUP.get().max(1) as u64;
    let mut groups: Vec<std::ops::Range<usize>> = Vec::new();
    let mut bytes = 0;
    for (i, &k) in order.iter().enumerate() {
        let l = slices[k].2;
        match groups.last_mut() {
            Some(g) if bytes + l <= solid => {
                g.end = i + 1;
                bytes += l;
            }
            _ => {
                groups.push(i..i + 1);
                bytes = l;
            }
        }
    }
    let made = aqr_common::pool::par_map(groups.len(), threads.min(groups.len()), |g| {
        let ks = &order[groups[g].clone()];
        let total: u64 = ks.iter().map(|&k| slices[k].2).sum();
        let mut d = Vec::with_capacity(total as usize);
        for &k in ks {
            let (s, a, l) = slices[k];
            d.extend_from_slice(&f.streams[s].data[a as usize..(a + l) as usize]);
        }
        let c = compress(&d);
        (c.len() < d.len() - d.len() / 32).then_some(c)
    });
    // (of each slice taken, its group's application, as it is to be pushed, and its output)
    let mut to: Vec<Option<(usize, usize)>> = vec![None; slices.len()];
    let base = f.shared.len();
    let mut splits = Vec::new();
    for (g, c) in groups.iter().zip(made) {
        let Some(c) = c else { continue };
        let ks = &order[g.clone()];
        let sizes: Vec<u64> = ks.iter().map(|&k| slices[k].2).collect();
        let len: u64 = sizes.iter().sum();
        TAKEN[0].fetch_add(len, Relaxed);
        TAKEN[1].fetch_add(c.len() as u64, Relaxed);
        TAKEN[2].fetch_add(1, Relaxed);
        let clen = c.len() as u64;
        let limit;
        let file = {
            let probe = aqr_lzma::formula::decoded(Node::slice(0, 0, clen), len);
            let Node::Apply(p) = &probe else { unreachable!() };
            limit = crate::memory::limit(p, &[clen], &[len], &vec![&c[..]]);
            f.push_as("lzma", Kind::Compressed, c)
        };
        let Node::Apply(mut a) = aqr_lzma::formula::decoded(file, len) else { unreachable!() };
        (a.memory_limit, a.task_limits) = (limit.instance, limit.tasks);
        for (index, &k) in ks.iter().enumerate() {
            to[k] = Some((base + splits.len(), index));
        }
        splits.push((*a, sizes));
    }
    if splits.is_empty() {
        return false;
    }
    {
        let [a, b, n] = [0, 1, 2].map(|k| TAKEN[k].load(Relaxed));
        eprintln!("aqr-compress: -Z lzma: {n} groups so far, {a} -> {b} bytes");
    }
    // (the slices of no bytes of the streams taken: nothing, so that nothing takes them)
    let mut k = 0;
    f.map_slices(&mut |stream, start, len| {
        let t = to[k].take();
        k += 1;
        match t {
            Some((shared, index)) => Node::Output { shared, index, len },
            None if len == 0 && stream < n && taken[stream] => Node::Concat(Vec::new()),
            None => Node::Slice { stream, start, len },
        }
    });
    for (a, sizes) in splits {
        f.split(a, sizes);
    }
    // (the applications made first: those before may take their outputs)
    let made = f.shared.len();
    let order: Vec<usize> = (base..made).chain(0..base).collect();
    f.reorder_shared(&order);
    // (the streams taken let go: no slice takes them any more)
    f.compact();
    true
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
