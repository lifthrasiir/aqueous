//! The code of the module (see `formula`, and `FORMAT.md`), which needs no aqr-formula, so that
//! aqr-modules builds it into its Wasm module.

use crate::dex;
use crate::{Result, corrupt};

/// The memory plan of the module ([`aqr_api::stack`]): what it takes beyond its initial memory
/// at most, of params and inputs of these lengths (the first `sections`) and a file of `len`
/// bytes (`None` where they are not of one): what `aqr_api::whole` copies, and what decoding
/// takes ([`dex::decode_plan`]).
pub fn plan(params: u64, inputs: &[u64], sections: &[u8], len: u64) -> Option<u64> {
    Some(aqr_api::stack::whole(params, &[], inputs) + dex::decode_plan(sections, len)?)
}

/// The module: the file of `len` bytes from `params` (none) and `inputs` (the streams of
/// [`dex::STREAM_NAMES`]).
pub fn decode(params: &[u8], inputs: &[&[u8]], len: u64) -> Result<Vec<u8>> {
    if !params.is_empty() {
        return Err(corrupt());
    }
    dex::decode(inputs, usize::try_from(len).map_err(|_| corrupt())?)
}
