//! The code of the module (see `formula`, and FORMAT.md), which needs no aqr-formula, so that
//! aqr-modules builds it into its Wasm module; and the encoder's helpers to run jobs in
//! parallel.

use crate::layout;
use crate::{Result, corrupt};

/// Bytes from which a DEFLATE stream gets threads of its own: two of aqr-flate's segments.
const LARGE: usize = 2 << 20;

/// `f(item, threads)` on every element of `items`, by `threads` threads, the largest by `size`
/// first: those of at least [`LARGE`] with all of them (aqr-flate codes segments in parallel,
/// but parses a stream on one), the others with one each, all side by side, so that the small
/// fill the threads a large one leaves idle.
pub(crate) fn par_jobs<T: Sync, R: Send>(
    items: &[T],
    size: impl Fn(&T) -> usize + Sync,
    threads: usize,
    f: impl Fn(&T, usize) -> R + Sync,
) -> Vec<R> {
    let mut order: Vec<usize> = (0..items.len()).collect();
    order.sort_by_key(|&i| std::cmp::Reverse(size(&items[i])));
    let done = par_map(&order, threads, |&i| f(&items[i], if size(&items[i]) >= LARGE { threads } else { 1 }));
    let mut out: Vec<Option<R>> = (0..items.len()).map(|_| None).collect();
    for (i, r) in order.into_iter().zip(done) {
        out[i] = Some(r);
    }
    out.into_iter().map(Option::unwrap).collect()
}

/// `f` on every element of `items`, on up to `threads` threads.
pub(crate) fn par_map<T: Sync, R: Send>(items: &[T], threads: usize, f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    aqr_common::pool::par_map(items.len(), threads, |i| f(&items[i]))
}

/// The module [`MODULE`]: the archive of `len` bytes rebuilt from `inputs`: the contents of
/// the stored bodies, the deflated bodies with what aqr-flate's module derives of them
/// (`layout::deflated`), the layout and the bytes kept.
pub fn decode(params: &[u8], inputs: &[&[u8]], len: u64, _threads: usize) -> Result<Vec<u8>> {
    let (&[], &[data, flate, layout, kept]) = (params, inputs) else { return Err(corrupt()) };
    layout::build(layout, data, flate, kept, len)
}
