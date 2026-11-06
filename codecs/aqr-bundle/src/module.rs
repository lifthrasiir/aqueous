//! The code of the module (see `formula`, and `FORMAT.md`), which needs no aqr-formula, so that
//! aqr-modules builds it into its Wasm module.

use crate::layout::{self};
use crate::{Result, corrupt};

/// The memory plan of the module ([`aqr_api::stack`]): what it takes beyond its initial memory
/// at most, of inputs of `inputs` bytes and an output of `len`: what `aqr_api::whole` copies, the
/// output, and a cpio member's header made apart on top of it.
pub fn plan(inputs: &[u64], len: u64) -> u64 {
    use aqr_api::stack::{block, whole};
    whole(0, &[], inputs) + block(len) + block(layout::Cpio::Newc.header() as u64)
}

/// The module: the archive of `len` bytes rebuilt from `inputs`, the contents and the layout.
pub fn decode(params: &[u8], inputs: &[&[u8]], len: u64, _threads: usize) -> Result<Vec<u8>> {
    let (&[], &[data, layout]) = (params, inputs) else { return Err(corrupt()) };
    layout::build(layout, data, len)
}
