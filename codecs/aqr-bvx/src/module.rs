//! The code of the module `bvx-recons` (see `formula`, and `FORMAT.md`), which needs no
//! aqr-formula, so that aqr-modules builds it into its Wasm module.

use crate::plan::{self, Counts, Part, Seg};
use crate::{Error, Result, bvx, corrupt, recons};

/// The memory plan of the module ([`aqr_api::stack`]): what it takes beyond its initial memory
/// at most, of params `params`, inputs of `inputs` bytes and an output of `len`: what
/// `aqr_api::whole` copies, the output and the plan's vectors, then the most of coding the
/// plan (the match finders' tables) and of writing the blocks (an LZFSE block's triples,
/// literals and tables). None where the params are not the module's.
pub fn plan(params: &[u8], inputs: &[u64], len: u64) -> Option<u64> {
    use aqr_api::stack::{block, whole};
    let c = Counts::read(params).ok()?;
    let vec = |n: usize, size: usize| Some(block((n as u64).checked_mul(size as u64)?));
    let held = [
        Some(block(len)),
        vec(c.parts, size_of::<Part>()),
        vec(c.segs, size_of::<Seg>()),
        vec(c.matches, size_of::<plan::Match>()),
        vec(c.freqs, size_of::<bvx::Freqs>()),
        vec(c.zd, 4),
    ];
    let held = held.into_iter().try_fold(0u64, |a, b| a.checked_add(b?))?;
    let code: u64 = recons::TABLES_HEAP.into_iter().map(block).sum();
    let write = match c.lmd {
        0 => 0,
        _ => plan::each_block_heap(&c).into_iter().chain(bvx::WRITE_V2_HEAP).map(block).sum(),
    };
    whole(params.len() as u64, &[], inputs).checked_add(held)?.checked_add(code.max(write))
}

/// The module: the stream of `len` bytes that the inputs (data, record) rebuild, of the plan's
/// counts in the params (see `FORMAT.md`).
pub fn decode(params: &[u8], inputs: &[&[u8]], len: u64, _threads: usize) -> Result<Vec<u8>> {
    let &[data, record] = inputs else { return Err(Error("two inputs wanted")) };
    let c = Counts::read(params)?;
    let len = usize::try_from(len).map_err(|_| corrupt())?;
    let mut out = Vec::with_capacity(len);
    recons::join_onto(data, record, &c, &mut out)?;
    if out.len() != len {
        return Err(Error("output of another length"));
    }
    Ok(out)
}
