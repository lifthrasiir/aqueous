//! The code of the module (see `formula`, and `FORMAT.md`), which needs no aqr-formula, so that
//! aqr-modules builds it into its Wasm module.

use crate::dex;
use crate::{Result, corrupt};

/// The module: the file of `len` bytes from `params` (none) and `inputs` (the streams of
/// [`dex::STREAM_NAMES`]).
pub fn decode(params: &[u8], inputs: &[&[u8]], len: u64) -> Result<Vec<u8>> {
    if !params.is_empty() {
        return Err(corrupt());
    }
    dex::decode(inputs, usize::try_from(len).map_err(|_| corrupt())?)
}
