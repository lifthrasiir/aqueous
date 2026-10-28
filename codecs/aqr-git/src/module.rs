//! The code of the module (see `formula`, and FORMAT.md), which needs no aqr-formula, so that
//! aqr-modules builds it into its Wasm module.

use crate::layout;
use crate::{Result, corrupt};

/// The module: the pack of `len` bytes rebuilt from `inputs`: the rebuilt entries' DEFLATE
/// streams with what aqr-flate's module derives of them, the layout and the bytes kept.
pub fn decode(params: &[u8], inputs: &[&[u8]], len: u64, _threads: usize) -> Result<Vec<u8>> {
    let (&[], &[deflated, layout, kept]) = (params, inputs) else { return Err(corrupt()) };
    layout::build(layout, deflated, kept, len)
}
