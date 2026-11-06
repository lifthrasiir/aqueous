//! The code of the module (see `formula`, and `FORMAT.md`), which needs no aqr-formula, so that
//! aqr-modules builds it into its Wasm module.

use crate::wasm::{self, STREAMS};
use crate::{Result, corrupt};

/// The memory plan of the module ([`aqr_api::stack`]): what it takes beyond its initial memory
/// at most, of inputs of `inputs` bytes and an output of `len`: what `aqr_api::whole` copies, and
/// what the module's join takes ([`wasm::join_plan`]).
pub fn plan(inputs: &[u64], len: u64) -> u64 {
    aqr_api::stack::whole(0, &[], inputs) + wasm::join_plan(len)
}

/// The module: the Wasm module of `len` bytes from `inputs` (the streams of
/// `wasm::STREAM_NAMES`); no params.
pub fn decode(params: &[u8], inputs: &[&[u8]], len: u64) -> Result<Vec<u8>> {
    let ins: [&[u8]; STREAMS] = inputs.try_into().map_err(|_| corrupt())?;
    if !params.is_empty() {
        return Err(corrupt());
    }
    wasm::join(ins, len)
}
