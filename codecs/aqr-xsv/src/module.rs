//! The code of the module (see `formula`, and `FORMAT.md`), which needs no aqr-formula, so that
//! aqr-modules builds it into its Wasm module.

use crate::table::{self, Dialect, View};
use crate::{Result, corrupt};

/// The module: the file of `len` bytes from `params` (the dialect and the number of columns)
/// and `inputs` (the streams of `formula::STREAM_NAMES`).
pub fn decode(params: &[u8], inputs: &[&[u8]], len: u64, _threads: usize) -> Result<Vec<u8>> {
    let &[layout, raw, columns, types, text, nums, desc] = inputs else { return Err(corrupt()) };
    let (d, ncols) = Dialect::from_params(params)?;
    let len = usize::try_from(len).map_err(|_| corrupt())?;
    table::build(&d, ncols, View { layout, raw, columns, types, text, nums, desc }, len)
}
