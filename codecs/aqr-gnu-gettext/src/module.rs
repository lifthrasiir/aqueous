//! The code of the module (see `formula`, and `FORMAT.md`), which needs no aqr-formula, so that
//! aqr-modules builds it into its Wasm module.

use crate::{Result, corrupt, mo};
use aqr_common::varint;

/// The memory plan of the module ([`aqr_api::stack`]): what it takes beyond its initial memory
/// at most, of `params`, inputs of `inputs` bytes (the first `rest`) and an output of `len`: what
/// `aqr_api::whole` copies, and what building the file takes ([`mo::build_plan`]).
pub fn plan(params: u64, inputs: &[u64], rest: &[u8], len: u64) -> Option<u64> {
    Some(aqr_api::stack::whole(params, &[], inputs) + mo::build_plan(rest, len)?)
}

/// The module: the file of `len` bytes from `params` (where the strings start, a varint) and
/// `inputs` (the file without the parts derived, and the strings' NULs).
pub fn decode(params: &[u8], inputs: &[&[u8]], len: u64) -> Result<Vec<u8>> {
    let &[rest, nuls] = inputs else { return Err(corrupt()) };
    let mut at = 0;
    let start = varint::get(params, &mut at).map_err(|_| corrupt())?;
    if at != params.len() {
        return Err(corrupt());
    }
    let (start, len) = (usize::try_from(start).map_err(|_| corrupt())?, usize::try_from(len).map_err(|_| corrupt())?);
    mo::build(rest, start, nuls, len)
}
