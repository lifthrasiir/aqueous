//! The code of the module (see `formula`, and `FORMAT.md`), which needs no aqr-formula, so that
//! aqr-modules builds it into its Wasm module.

use crate::wasm::{self, STREAMS};
use crate::{Result, corrupt};

/// The module: the Wasm module of `len` bytes from `inputs` (the streams of
/// `wasm::STREAM_NAMES`); no params.
pub fn decode(params: &[u8], inputs: &[&[u8]], len: u64) -> Result<Vec<u8>> {
    let ins: [&[u8]; STREAMS] = inputs.try_into().map_err(|_| corrupt())?;
    if !params.is_empty() {
        return Err(corrupt());
    }
    wasm::join(ins, len)
}
