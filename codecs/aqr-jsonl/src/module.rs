//! The code of the module (see `formula`, and `FORMAT.md`), which needs no aqr-formula, so that
//! aqr-modules builds it into its Wasm module.

use crate::tree::{self, View};
use crate::{Result, corrupt};

/// The memory plan of the module ([`aqr_api::stack`]): what it takes beyond its initial memory
/// at most, of params `params`, inputs of `inputs` bytes, the streams `paths` and `keys`, and a
/// file of `len` bytes ([`tree::plan`]).
pub fn plan(params: &[u8], inputs: &[u64], paths: &[u8], keys: &[u8], len: u64) -> Option<u64> {
    Some(aqr_api::stack::whole(params.len() as u64, &[], inputs) + tree::plan(params, paths, keys, len)?)
}

/// The module: the file of `len` bytes from `params` (the style and the key lists defined) and `inputs` (the streams of
/// `formula::STREAM_NAMES`).
pub fn decode(params: &[u8], inputs: &[&[u8]], len: u64, _threads: usize) -> Result<Vec<u8>> {
    let &[layout, raw, paths, keys, types, shapes, names, text, nums, desc] = inputs else { return Err(corrupt()) };
    let len = usize::try_from(len).map_err(|_| corrupt())?;
    tree::build(params, View { layout, raw, paths, keys, types, shapes, names, text, nums, desc }, len)
}
