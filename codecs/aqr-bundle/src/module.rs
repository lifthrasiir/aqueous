//! The code of the module (see `formula`, and `FORMAT.md`), which needs no aqr-formula, so that
//! aqr-modules builds it into its Wasm module.

use crate::layout::{self};
use crate::{Result, corrupt};

/// The module: the archive of `len` bytes rebuilt from `inputs`, the contents and the layout.
pub fn decode(params: &[u8], inputs: &[&[u8]], len: u64, _threads: usize) -> Result<Vec<u8>> {
    let (&[], &[data, layout]) = (params, inputs) else { return Err(corrupt()) };
    layout::build(layout, data, len)
}
