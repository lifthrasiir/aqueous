//! The code of the module (see `formula`, and `FORMAT.md`), which needs no aqr-formula, so that
//! aqr-modules builds it into its Wasm module.

use crate::table::{self, Dialect, View};
use crate::{Result, corrupt};

/// The module: the file of `len` bytes from `params` (the dialect and the number of columns)
/// and `inputs` (the streams of `formula::STREAM_NAMES`).
/// The memory plan of the module ([`aqr_api::stack`]): what it takes beyond its initial memory
/// at most, of `params`, inputs of `inputs` bytes and an output of `len`: what `aqr_api::whole`
/// copies, the dialect's delimiter and terminator, and what building the table takes
/// ([`table::build_plan`]). `None` for params it does not take.
pub fn plan(params: &[u8], inputs: &[u64], len: u64) -> Option<u64> {
    use aqr_api::stack::{block, whole};
    let (d, ncols) = Dialect::from_params(params).ok()?;
    let dialect = block(d.delim.len() as u64) + block(d.term.len() as u64);
    Some(whole(params.len() as u64, &[], inputs) + dialect + table::build_plan(ncols, len))
}

pub fn decode(params: &[u8], inputs: &[&[u8]], len: u64, _threads: usize) -> Result<Vec<u8>> {
    let &[layout, raw, columns, types, text, nums, desc] = inputs else { return Err(corrupt()) };
    let (d, ncols) = Dialect::from_params(params)?;
    let len = usize::try_from(len).map_err(|_| corrupt())?;
    table::build(&d, ncols, View { layout, raw, columns, types, text, nums, desc }, len)
}
