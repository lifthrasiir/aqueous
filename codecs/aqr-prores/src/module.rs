//! The code of the module `prores-recons` (see `formula`, and `FORMAT.md`), which needs no
//! aqr-formula, so that aqr-modules builds it into its Wasm module.

use aqr_common::varint;

use crate::codec::{self, Counts, Limits, MAX_STRIPES};
use crate::{Error, Result};

/// Most blocks of a frame and bytes of a frame that parameters may give (8K 4:4:4 takes about
/// 1.6 M blocks; a frame's size is a u32).
const MAX_BLOCKS: u64 = 1 << 24;
const MAX_FRAME: u64 = u32::MAX as u64;

/// The parameters of an application: the limits of its chunk, as varints (the stripes' results
/// after their number, then the counts of what the first stream tells).
pub fn params(l: &Limits) -> Vec<u8> {
    let mut p = Vec::new();
    let k = l.results.len() as u64;
    let head = [l.blocks, l.frame, l.side, l.stripe_blocks, k];
    for v in head.into_iter().chain(l.results.iter().copied()).chain(l.counts.to_array()) {
        varint::put(&mut p, v);
    }
    p
}

/// The limits that parameters `p` give.
pub fn limits(p: &[u8]) -> Result<Limits> {
    let bad = || Error("bad parameters");
    let mut at = 0;
    let mut v = || varint::get(p, &mut at).map_err(|_| bad());
    let (blocks, frame, side, stripe_blocks, k) = (v()?, v()?, v()?, v()?, v()?);
    if !(1..=MAX_STRIPES as u64).contains(&k) {
        return Err(bad());
    }
    let results = (0..k).map(|_| v()).collect::<Result<Vec<_>>>()?;
    let mut counts = [0; 6];
    for c in &mut counts {
        *c = v()?;
    }
    let counts = Counts::from_array(counts);
    if at != p.len() || blocks > MAX_BLOCKS || stripe_blocks > blocks || frame > MAX_FRAME {
        return Err(bad());
    }
    Ok(Limits { blocks, frame, side, stripe_blocks, results, counts })
}

/// The task slots of the module (aqr-modules' `prores-recons`), and so its threads.
pub const SLOTS: usize = 16;

/// The memory plan of the module ([`aqr_api::stack`]): the most its instance and tasks take at
/// once, its initial memory (`initial` bytes) with it, of params `params`, inputs of `inputs`
/// bytes, the coded chunk `coded` (the first input) and a span of `len` bytes (`None` where they
/// are not so): what `aqr_api::whole` copies, and what decoding takes ([`codec::plan`]).
pub fn plan(params: &[u8], inputs: &[u64], coded: &[u8], initial: u64, len: u64) -> Option<u64> {
    let l = limits(params).ok()?;
    let held = aqr_api::stack::whole(params.len() as u64, &[], inputs);
    codec::plan(&l, coded, len, initial, held, SLOTS)
}

/// The module: the span of `len` bytes that the inputs (the coded chunk, the bytes between its
/// frames) rebuild (see `FORMAT.md`).
pub fn decode(params: &[u8], inputs: &[&[u8]], len: u64, threads: usize) -> Result<Vec<u8>> {
    let &[coded, gaps] = inputs else { return Err(Error("two inputs wanted")) };
    let len = usize::try_from(len).map_err(|_| Error("output too large"))?;
    codec::decode(coded, gaps, &limits(params)?, len, threads)
}
