//! The code of the module (see `formula`, and `FORMAT.md`), which needs no aqr-formula, so that
//! aqr-modules builds it into its Wasm module.

use crate::{Result, corrupt, layout};

/// The memory plan of the module ([`aqr_api::stack`]): what it takes beyond its initial memory
/// at most, of inputs of `inputs` bytes (the third the index `index`) and an output of `len`: what
/// `aqr_api::whole` copies, the bodies the index tells (16 bytes each), and the output. `None`
/// where the index does not tell how many bodies.
pub fn plan(inputs: &[u64], index: &[u8], len: u64) -> Option<u64> {
    use aqr_api::stack::{block, whole};
    let n = aqr_common::varint::get(index, &mut 0).ok()?;
    Some(whole(0, &[], inputs) + block(16 * n.min(index.len() as u64 / 2)) + block(len))
}

/// The module: the archive of `len` bytes rebuilt from `inputs`, the data, the header and the
/// index.
pub fn decode(params: &[u8], inputs: &[&[u8]], len: u64, _threads: usize) -> Result<Vec<u8>> {
    let (&[], &[data, header, index]) = (params, inputs) else { return Err(corrupt()) };
    layout::build(data, header, index, len)
}
