//! The code of the module `prores-recons` (see `formula`, and `FORMAT.md`), which needs no
//! aqr-formula, so that aqr-modules builds it into its Wasm module.

use aqr_common::varint;

use crate::codec::{self, Limits, STRIPES};
use crate::{Error, Result};

/// Most blocks of a frame and bytes of a frame that parameters may give (8K 4:4:4 takes about
/// 1.6 M blocks; a frame's size is a u32).
const MAX_BLOCKS: u64 = 1 << 24;
const MAX_FRAME: u64 = u32::MAX as u64;

/// The parameters of an application: the limits of its chunk, as varints.
pub fn params(l: Limits) -> Vec<u8> {
    let mut p = Vec::new();
    for v in [l.blocks, l.frame, l.side, l.stripe_blocks].into_iter().chain(l.results) {
        varint::put(&mut p, v);
    }
    p
}

/// The limits that parameters `p` give.
pub fn limits(p: &[u8]) -> Result<Limits> {
    let bad = || Error("bad parameters");
    let mut at = 0;
    let mut v = || varint::get(p, &mut at).map_err(|_| bad());
    let (blocks, frame, side, stripe_blocks) = (v()?, v()?, v()?, v()?);
    let mut results = [0; STRIPES];
    for r in &mut results {
        *r = v()?;
    }
    if at != p.len() || blocks > MAX_BLOCKS || stripe_blocks > blocks || frame > MAX_FRAME {
        return Err(bad());
    }
    Ok(Limits { blocks, frame, side, stripe_blocks, results })
}

/// The module: the span of `len` bytes that the inputs (the coded chunk, the bytes between its
/// frames) rebuild (see `FORMAT.md`).
pub fn decode(params: &[u8], inputs: &[&[u8]], len: u64, threads: usize) -> Result<Vec<u8>> {
    let &[coded, gaps] = inputs else { return Err(Error("two inputs wanted")) };
    let len = usize::try_from(len).map_err(|_| Error("output too large"))?;
    codec::decode(coded, gaps, limits(params)?, len, threads)
}
