//! What Zstandard frames hold, taken out of them in a formula (see aqr-formula): each frame's
//! blocks decompressed by a Patch instruction of an empty source, its output split
//! ([`Formula::split_patch`]) into the pieces wanted and those between them, which are dropped
//! (the source of a Patch instruction of an empty block). `FORMAT.md` describes it.

use std::ops::Range;

use aqr_formula::{Formula, Node, Patch};

/// The patch of an empty output: a raw block of no bytes, the final one.
pub const EMPTY: [u8; 3] = [1, 0, 0];

/// The nodes giving the ranges `wanted` of the content of the frames `frames` (each the bytes
/// of its blocks in stream `stream` of `f`, and the size of its content, one after another),
/// in order and apart. Each frame that a range is in is decompressed once, its pieces taken
/// by the ranges; those between them are dropped by the node of the range after them (by the
/// last one, after it). Frames holding none are left out.
pub fn take(f: &mut Formula, stream: usize, frames: &[(Range<u64>, u64)], wanted: &[Range<u64>]) -> Vec<Node> {
    debug_assert!(wanted.windows(2).all(|w| w[0].end <= w[1].start) && wanted.iter().all(|w| w.start < w.end));
    debug_assert!(wanted.last().is_none_or(|w| w.end <= frames.iter().map(|f| f.1).sum()));
    // of each range: the pieces dropped before it, and its own
    let mut takes: Vec<(Vec<Node>, Vec<Node>)> = vec![Default::default(); wanted.len()];
    let mut dropped: Vec<Node> = Vec::new();
    let empty = f.push("zstd/empty", EMPTY.to_vec());
    let mut at = 0u64;
    // the first range that the frame holds or that comes after it
    let mut w = 0;
    for (blocks, len) in frames {
        let (start, end) = (at, at + len);
        at = end;
        if wanted.get(w).is_none_or(|r| r.start >= end) {
            continue;
        }
        // its pieces: to whom each goes (the range, or none)
        let mut pieces: Vec<(u64, Option<usize>)> = Vec::new();
        let mut p = start;
        let mut k = w;
        while p < end {
            match wanted.get(k) {
                Some(r) if r.start <= p => {
                    let e = r.end.min(end);
                    pieces.push((e - p, Some(k)));
                    p = e;
                    if e == r.end {
                        k += 1;
                    }
                }
                Some(r) if r.start < end => {
                    pieces.push((r.start - p, None));
                    p = r.start;
                }
                _ => {
                    pieces.push((end - p, None));
                    p = end;
                }
            }
        }
        w = k;
        let patch = Node::slice(stream, blocks.start, blocks.end - blocks.start);
        let outputs = f.split_patch(Node::concat([]), patch, pieces.iter().map(|p| p.0).collect());
        for (o, &(_, to)) in outputs.into_iter().zip(&pieces) {
            match to {
                Some(k) => {
                    takes[k].0.append(&mut dropped);
                    takes[k].1.push(o);
                }
                None => dropped.push(o),
            }
        }
    }
    let drop = |ns: Vec<Node>| Node::Patch(Box::new(Patch { source: Node::concat(ns), patch: empty.clone(), len: 0 }));
    // (what is left after the last range, dropped after it)
    let n = takes.len();
    takes
        .into_iter()
        .enumerate()
        .map(|(k, (before, own))| {
            let mut parts = Vec::new();
            if !before.is_empty() {
                parts.push(drop(before));
            }
            parts.extend(own);
            if k + 1 == n && !dropped.is_empty() {
                parts.push(drop(std::mem::take(&mut dropped)));
            }
            Node::concat(parts)
        })
        .collect()
}

/// A file of bytes kept and of frames made again of their contents ([`frames_in`]), a part
/// module (`crate::frames`).
pub const FRAMES: aqr_formula::Module = aqr_formula::Module::new("zstd-frames", 1).with_plan(frames_memory);

fn frames_memory(u: &aqr_formula::Usage<'_>) -> Option<u64> {
    crate::frames::plan(u.initial(), u.params)
}

/// Frames checked at once, about: their bytes (their contents some 10 times that at most).
const BATCH: usize = 16 << 20;

/// Bytes of the frames open at once at most (of the module's pool, which the first fit of its
/// blocks makes larger), and of the pool: so that the module's plan stays well within the bound
/// of an application (256 MiB compressing).
const OPEN_BYTES: u64 = 96 << 20;
const POOL_BYTES: u64 = 160 << 20;

/// The frames of `order` (by their first pieces) to be left out so that those open at once (from
/// their first piece to their last) are of `budget` bytes at most: where more would be, the one
/// closing last is (the greedy choice leaving out the fewest).
fn over_budget(frames: &[Vec<Range<usize>>], order: &[usize], budget: u64) -> std::collections::HashSet<usize> {
    use std::cmp::Reverse;
    use std::collections::BinaryHeap;
    let span = |i: usize| {
        let lo = frames[i].iter().map(|r| r.start).min().unwrap_or(0);
        let hi = frames[i].iter().map(|r| r.end).max().unwrap_or(0);
        (lo, hi, frames[i].iter().map(|r| r.len() as u64).sum::<u64>())
    };
    let mut out = std::collections::HashSet::new();
    let (mut last, mut first) = (BinaryHeap::new(), BinaryHeap::new());
    let mut open = 0u64;
    let mut gone = vec![false; frames.len()];
    for &i in order {
        let (lo, hi, n) = span(i);
        while let Some(&Reverse((h, j))) = first.peek() {
            if h > lo {
                break;
            }
            first.pop();
            if !std::mem::replace(&mut gone[j], true) {
                open -= span(j).2;
            }
        }
        last.push((hi, i));
        first.push(Reverse((hi, i)));
        open += n;
        while open > budget {
            let Some((_, j)) = last.pop() else { break };
            if !std::mem::replace(&mut gone[j], true) {
                open -= span(j).2;
                out.insert(j);
            }
        }
    }
    out
}

/// The formula making `input` again of the frames among `frames` (each the pieces of the file it
/// is, in order) that libzstd wrote at `level` as [`crate::compress`] emulates it, their
/// contents stored instead (as a stream of `kind`), and of the bytes around them kept: None where
/// none is. A frame is taken where its pieces are apart from the others' (in any order in the
/// file). The frames are decompressed and made again `threads` at a time, a batch at a time;
/// the contents are written to the store as they come ([`aqr_formula::store::Writer`]).
pub fn frames_in(
    input: &[u8],
    frames: &[Vec<Range<usize>>],
    level: i32,
    kind: aqr_formula::Kind,
    threads: usize,
) -> Option<Formula> {
    let mut contents = aqr_formula::store::Writer::new(None);
    let mut writer = Some(&mut contents);
    let mut f = frames_core(input, frames, level, threads, &mut writer)?.0;
    let node = f.push_as("zstd/contents", kind, contents);
    let aqr_formula::Node::Apply(a) = &mut f.root else { unreachable!("an application") };
    a.inputs.push(node);
    Some(f)
}

/// The frames that [`frames_in_by`] takes: each made again of its content, in the order of the
/// contents (the frames' indices), and their contents' bytes.
#[derive(Debug, Clone, Default)]
pub struct Taken {
    pub order: Vec<usize>,
    pub lens: Vec<u64>,
}

/// [`frames_in`], its contents (one after another, as `taken` has them) the node that `contents`
/// gives of the formula (its streams pushed to it): None where it gives none.
pub fn frames_in_by(
    input: &[u8],
    frames: &[Vec<Range<usize>>],
    level: i32,
    threads: usize,
    contents: &mut dyn FnMut(&Taken, &mut Formula) -> Option<Node>,
) -> Option<Formula> {
    let (mut f, taken) = frames_core(input, frames, level, threads, &mut None)?;
    let node = contents(&taken, &mut f)?;
    let aqr_formula::Node::Apply(a) = &mut f.root else { unreachable!("an application") };
    a.inputs.push(node);
    Some(f)
}

/// The formula of [`frames_in`] but its last input, the contents (written to `writer` where
/// given, as they are checked), and the frames taken.
fn frames_core(
    input: &[u8],
    frames: &[Vec<Range<usize>>],
    level: i32,
    threads: usize,
    writer: &mut Option<&mut aqr_formula::store::Writer>,
) -> Option<(Formula, Taken)> {
    use crate::frames::{Frames, KEPT, NEW, PIECE, Params, SHIFT, zigzag};
    use aqr_common::varint::put;
    crate::compress::params(level, 0)?;
    // the frames by their first piece in the file, their pieces in it and apart
    let fits = |p: &[Range<usize>]| !p.is_empty() && p.iter().all(|r| r.start < r.end && r.end <= input.len());
    let mut order: Vec<usize> = (0..frames.len()).filter(|&i| fits(&frames[i])).collect();
    let first = |i: usize| frames[i].iter().map(|r| r.start).min();
    order.sort_by_key(|&i| first(i));
    {
        let mut pieces: Vec<(usize, usize, usize)> =
            order.iter().flat_map(|&i| frames[i].iter().map(move |r| (r.start, r.end, i))).collect();
        pieces.sort_unstable();
        let mut bad = std::collections::HashSet::new();
        for w in pieces.windows(2) {
            if w[0].1 > w[1].0 {
                bad.insert(w[0].2);
                bad.insert(w[1].2);
            }
        }
        order.retain(|i| !bad.contains(i));
    }
    // (of a pool of POOL_BYTES at most: the frames open at once fewer until it is)
    let mut budget = OPEN_BYTES;
    loop {
        let left = over_budget(frames, &order, budget);
        let mut sim = crate::frames::Frames::new(&Params { level: level as u8, content: 0, pool: u64::MAX, open: 0 });
        let kept: Vec<usize> = order.iter().copied().filter(|i| !left.contains(i)).collect();
        if pool_of(frames, &kept, &mut sim) <= POOL_BYTES || budget < 1 << 20 {
            order = kept;
            break;
        }
        budget /= 2;
    }
    // each frame decompressed and made again, a batch at a time: the contents of those made
    let mut taken_order = Taken::default();
    let mut lengths = Vec::new();
    let mut taken = vec![None::<u64>; frames.len()];
    let mut max_content = 0u64;
    let mut at = 0;
    while at < order.len() {
        let mut end = at;
        let mut bytes = 0;
        while end < order.len() && bytes < BATCH {
            bytes += frames[order[end]].iter().map(|r| r.len()).sum::<usize>();
            end += 1;
        }
        let batch = &order[at..end];
        // (in groups of frames, each of a compressor and a decompressor of its own)
        const GROUP: usize = 256;
        let made: Vec<Vec<Option<Vec<u8>>>> = aqr_common::pool::par_map(batch.len().div_ceil(GROUP), threads, |g| {
            let mut z = crate::compress::Compressor::new(crate::compress::CONTENT_MAX);
            let mut dec = zstd::bulk::Decompressor::new().ok();
            let (mut frame, mut made) = (Vec::new(), Vec::new());
            batch[g * GROUP..(g * GROUP + GROUP).min(batch.len())]
                .iter()
                .map(|&i| {
                    frame.clear();
                    frames[i].iter().for_each(|r| frame.extend_from_slice(&input[r.clone()]));
                    let c = dec.as_mut()?.decompress(&frame, crate::compress::CONTENT_MAX).ok()?;
                    made.clear();
                    (z.compress(&c, level, &mut made) && made == frame).then_some(c)
                })
                .collect()
        });
        for (k, c) in made.into_iter().flatten().enumerate() {
            if let Some(c) = c {
                max_content = max_content.max(c.len() as u64);
                taken[batch[k]] = Some(c.len() as u64);
                put(&mut lengths, c.len() as u64);
                taken_order.order.push(batch[k]);
                taken_order.lens.push(c.len() as u64);
                if let Some(w) = writer.as_mut() {
                    w.write(&c);
                }
            }
        }
        at = end;
    }
    if taken.iter().all(|t| t.is_none()) {
        return None;
    }
    // the layout, in the order of the file; the frames open as the module has them
    // (each piece, its frame and where in it it starts)
    let mut pieces: Vec<(Range<usize>, usize, u64)> = Vec::new();
    for &i in order.iter().filter(|&&i| taken[i].is_some()) {
        let mut o = 0;
        for r in &frames[i] {
            pieces.push((r.clone(), i, o));
            o += r.len() as u64;
        }
    }
    pieces.sort_unstable_by_key(|p| p.0.start);
    let mut next = vec![0u64; frames.len()];
    let mut started = vec![false; frames.len()];
    let lens: Vec<u64> = frames.iter().map(|p| p.iter().map(|r| r.len() as u64).sum()).collect();
    let mut sim = Frames::new(&Params { level: level as u8, content: 0, pool: u64::MAX, open: 0 });
    let mut open: Vec<usize> = Vec::new();
    let mut layout = Vec::new();
    let mut kept = aqr_formula::store::Writer::new(None);
    let mut done = 0;
    for &(ref r, i, o) in &pieces {
        if done < r.start {
            put(&mut layout, ((r.start - done) as u64) << SHIFT | KEPT);
            kept.write(&input[done..r.start]);
        }
        let n = r.len() as u64;
        let delta = o as i64 - next[i] as i64;
        next[i] = o + n;
        if !std::mem::replace(&mut started[i], true) {
            put(&mut layout, n << SHIFT | NEW);
            put(&mut layout, zigzag(delta));
            sim.new_frame(lens[i], u64::MAX)?;
            sim.piece(0, delta, n)?;
            if n < lens[i] {
                open.push(i);
            }
        } else {
            let j = open.iter().rev().position(|&f| f == i)?;
            put(&mut layout, n << SHIFT | PIECE);
            put(&mut layout, j as u64);
            put(&mut layout, zigzag(delta));
            sim.piece(j as u64, delta, n)?;
            if sim.open.len() < open.len() {
                open.retain(|&f| f != i);
            }
        }
        done = r.end;
    }
    if done < input.len() {
        put(&mut layout, ((input.len() - done) as u64) << SHIFT | KEPT);
        kept.write(&input[done..]);
    }
    if !sim.open.is_empty() {
        return None;
    }
    let params = Params { level: level as u8, content: max_content, pool: sim.pool.most, open: sim.most as u64 };
    let mut f = Formula::default();
    f.push("raw", Vec::new());
    let layout = f.push_as("zstd/layout", aqr_formula::Kind::Table, layout);
    let kept = f.push("zstd/kept", kept);
    let lengths = f.push_as("zstd/lengths", aqr_formula::Kind::Table, lengths);
    f.root = Node::apply(FRAMES, params.bytes(), vec![layout, kept, lengths], input.len() as u64);
    Some((f, taken_order))
}

/// The output of `a`, an application of [`FRAMES`], on `inputs`, on up to `threads` threads.
pub fn apply(a: &aqr_formula::Apply, inputs: &[&[u8]], threads: usize) -> Option<Vec<u8>> {
    (a.module == FRAMES && a.resources.is_empty())
        .then(|| crate::frames::decode(&a.params, inputs, a.len, threads).ok())?
}

/// The most of the pool that the frames of `order` take (all of them made of their contents), the
/// frames open as the module has them (`sim`).
fn pool_of(frames: &[Vec<Range<usize>>], order: &[usize], sim: &mut crate::frames::Frames) -> u64 {
    // (each piece: where it is, its frame, where in it it starts, its bytes)
    let mut pieces: Vec<(usize, usize, u64, u64)> = Vec::new();
    for &i in order {
        let mut o = 0;
        for r in &frames[i] {
            pieces.push((r.start, i, o, r.len() as u64));
            o += r.len() as u64;
        }
    }
    pieces.sort_unstable();
    let mut lens = vec![0u64; frames.len()];
    let mut next = vec![0u64; frames.len()];
    let mut started = vec![false; frames.len()];
    for &i in order {
        lens[i] = frames[i].iter().map(|r| r.len() as u64).sum();
    }
    let mut open: Vec<usize> = Vec::new();
    for (_, i, o, n) in pieces {
        let delta = o as i64 - next[i] as i64;
        next[i] = o + n;
        if !std::mem::replace(&mut started[i], true) {
            let _ = sim.new_frame(lens[i], u64::MAX);
            let _ = sim.piece(0, delta, n);
            if n < lens[i] {
                open.push(i);
            }
        } else if let Some(j) = open.iter().rev().position(|&f| f == i) {
            let _ = sim.piece(j as u64, delta, n);
            if sim.open.len() < open.len() {
                open.retain(|&f| f != i);
            }
        }
    }
    sim.pool.most
}
