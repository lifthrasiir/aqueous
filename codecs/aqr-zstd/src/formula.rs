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
