//! Streams cut into non-leaf blocks of [`BLOCK`] bytes, and the slices of a formula over them
//! (or literals, where short).

use super::*;

/// Bytes of a non-leaf block at most: a longer stream is several (`Encoder::stream_blocks`),
/// as the spec has content larger than a unit of random access split into blocks. As large as
/// the window of long-distance matching (which every decoder takes): of boot.wim's 494 MB of
/// data, 32 MiB blocks archived 3.1% larger, 64 MiB 2.7%, 128 MiB 0.5%, and decoding held
/// 0.39, 0.48 and 0.71 GB at most (0.66 GB of a block). 1 MiB in this crate's tests.
#[cfg(not(test))]
pub const BLOCK: usize = 128 << 20;
#[cfg(test)]
pub const BLOCK: usize = 1 << 20;

/// Non-leaf blocks of a stream of `len` bytes (one at least).
pub(crate) fn blocks_in(len: usize) -> u64 {
    len.div_ceil(BLOCK).max(1) as u64
}

/// The blocks of each of `streams` streams (of `len_of` bytes) that the slices below `nodes`
/// are over, by stream and block; none for a stream not sliced.
pub(crate) fn sliced<'a>(
    streams: usize,
    len_of: impl Fn(usize) -> usize,
    nodes: impl IntoIterator<Item = &'a Node>,
) -> Vec<Vec<bool>> {
    let mut used: Vec<Vec<bool>> = vec![Vec::new(); streams];
    for n in nodes {
        n.visit(&mut |n| {
            if let &Node::Slice { stream, start, len: len @ 1.. } = n {
                let u = &mut used[stream];
                if u.is_empty() {
                    *u = vec![false; blocks_in(len_of(stream)) as usize];
                }
                slice_blocks(start, len).into_iter().for_each(|(k, _, _)| u[k as usize] = true);
            }
        });
    }
    used
}

aqr_common::zopt! {
    /// The most bytes that the slices of a stream take, all of them, for the stream to be no
    /// block: its slices are Literal instructions of the formula then ([`literals`]). A small
    /// block costs its header and a Zstandard frame of its own, which compresses little; the
    /// formula block compresses the literals with the rest. Of 1,024 bytes: an installer of a
    /// CAB (Adobe Reader 9.4) 7.9 KB smaller, the CHM, ALZ and EGG samples and 61 small files
    /// 0.9 KB in all; from 4 KiB on the EGG samples grew (streams that compress better at the
    /// levels picked for them than in the formula block).
    pub(crate) static LITERAL_MAX: Opt<usize> =
        Opt::size("formula:literal-max", LITERAL_MAX_DEFAULT, "most bytes of a stream's slices written as literals of the formula, not a block");
}

/// [`LITERAL_MAX`] by default; none in this crate's tests, which look at the blocks of small
/// streams (those of literals set it).
#[cfg(not(test))]
pub(crate) const LITERAL_MAX_DEFAULT: usize = 1 << 10;
#[cfg(test)]
pub(crate) const LITERAL_MAX_DEFAULT: usize = 0;

/// Of each of `streams` streams, whether the slices below `nodes` are literals, not slices of
/// its blocks: where they take [`LITERAL_MAX`] bytes at most, all of them (as many times as
/// they are emitted), and `can` takes the stream.
pub(crate) fn literals<'a>(
    streams: usize,
    can: impl Fn(usize) -> bool,
    nodes: impl IntoIterator<Item = &'a Node>,
) -> Vec<bool> {
    let max = LITERAL_MAX.get() as u64;
    let mut taken = vec![0u64; streams];
    for n in nodes {
        n.visit(&mut |n| {
            if let &Node::Slice { stream, len, .. } = n {
                taken[stream] = taken[stream].saturating_add(len);
            }
        });
    }
    (0..streams).map(|s| taken[s] > 0 && taken[s] <= max && can(s)).collect()
}

/// The index in a formula block's list of each block `used` ([`sliced`]), those of the streams
/// in `order` one after another; and how many are listed.
pub(crate) fn listed(used: &[Vec<bool>], order: impl IntoIterator<Item = usize>) -> (Vec<Vec<Option<u64>>>, u64) {
    let mut ordinals: Vec<Vec<Option<u64>>> = used.iter().map(|u| vec![None; u.len()]).collect();
    let mut next = 0;
    for st in order {
        for (o, &u) in ordinals[st].iter_mut().zip(&used[st]) {
            if u {
                *o = Some(next);
                next += 1;
            }
        }
    }
    (ordinals, next)
}

/// The ranges of the non-leaf blocks of a stream of `len` bytes.
pub(crate) fn block_ranges(len: usize) -> impl Iterator<Item = Range<usize>> {
    (0..blocks_in(len) as usize).map(move |k| k * BLOCK..((k + 1) * BLOCK).min(len))
}

/// The pieces of a slice of a stream at `start` of `len` bytes (one at least): of each block
/// it is over, the block's index in the stream and the start and length in it.
pub(crate) fn slice_blocks(start: u64, len: u64) -> Vec<(u64, u64, u64)> {
    let b = BLOCK as u64;
    if len == 0 {
        return vec![(start / b, start % b, 0)];
    }
    let (first, last) = (start / b, (start + len - 1) / b);
    (first..=last)
        .map(|k| {
            let (s, e) = (start.max(k * b), (start + len).min((k + 1) * b));
            (k, s - k * b, e - s)
        })
        .collect()
}

/// The `len` bytes from `start` of `pieces`, as pieces.
pub(crate) fn cut<'a>(pieces: &[&'a [u8]], mut start: usize, mut len: usize) -> Vec<&'a [u8]> {
    let mut out = Vec::new();
    for p in pieces {
        if len == 0 {
            break;
        }
        if start >= p.len() {
            start -= p.len();
            continue;
        }
        let n = len.min(p.len() - start);
        out.push(&p[start..][..n]);
        (start, len) = (0, len - n);
    }
    out
}
