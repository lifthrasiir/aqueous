//! The code of the module `msadpcm-recons` (FORMAT.md §3), which needs no aqr-formula, so that
//! aqr-modules builds it into its Wasm module.

use aqr_common::varint;

use crate::codec::{self, Layout};
use crate::{Error, Result};

fn corrupt() -> Error {
    Error("corrupt input")
}

/// The params of blocks of `l`: its channels and its bytes per block, varints.
pub fn params(l: Layout) -> Vec<u8> {
    let mut p = Vec::new();
    varint::put(&mut p, l.channels as u64);
    varint::put(&mut p, l.block as u64);
    p
}

/// The layout of `params`, if they are exactly one.
fn layout(params: &[u8]) -> Option<Layout> {
    let mut at = 0;
    let channels = varint::get(params, &mut at).ok()?;
    let block = varint::get(params, &mut at).ok()?;
    if at != params.len() {
        return None;
    }
    Layout::new(usize::try_from(channels).ok()?, usize::try_from(block).ok()?)
}

/// The memory plan of the module ([`aqr_api::stack`]): what it takes beyond its initial memory
/// at most, of `params`, a record of `record` bytes and an output of `len`: what `aqr_api::whole`
/// copies, and what decoding takes ([`codec::decode_plan`]). `None` for params it does not take.
pub fn plan(params: &[u8], record: u64, len: u64) -> Option<u64> {
    let l = layout(params)?;
    Some(aqr_api::stack::whole(params.len() as u64, &[], &[record]) + codec::decode_plan(l, len))
}

/// The module: the blocks, `len` bytes, from `params` (their layout) and `inputs` (the record).
pub fn decode(params: &[u8], inputs: &[&[u8]], len: u64) -> Result<Vec<u8>> {
    let &[record] = inputs else { return Err(corrupt()) };
    let l = layout(params).ok_or(corrupt())?;
    codec::decode(l, record, usize::try_from(len).map_err(|_| corrupt())?)
}
