//! The code of the module (see `formula`, and `FORMAT.md`), which needs no aqr-formula, so that
//! aqr-modules builds it into its Wasm module.

use crate::tree::{self, Style, View};
use crate::{Result, corrupt};

/// The module: the file of `len` bytes from `params` (the style) and `inputs` (the streams of
/// `formula::STREAM_NAMES`).
pub fn decode(params: &[u8], inputs: &[&[u8]], len: u64, _threads: usize) -> Result<Vec<u8>> {
    let &[layout, raw, paths, keys, types, shapes, names, text, nums, desc] = inputs else { return Err(corrupt()) };
    let style = Style::from_params(params)?;
    let len = usize::try_from(len).map_err(|_| corrupt())?;
    tree::build(style, View { layout, raw, paths, keys, types, shapes, names, text, nums, desc }, len)
}
