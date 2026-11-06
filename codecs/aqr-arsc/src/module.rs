//! The code of the module (see `formula`, and `FORMAT.md`), which needs no aqr-formula, so that
//! aqr-modules builds it into its Wasm module.

use crate::arsc;
use crate::{Result, corrupt};

/// The memory plan of the module ([`aqr_api::stack`]): what it takes beyond its initial memory
/// at most, of params and inputs of these lengths (the first `head`) and a file of `len` bytes
/// (`None` where they are not of one): what `aqr_api::whole` copies, and what decoding takes
/// ([`arsc::decode_plan`]).
pub fn plan(params: u64, inputs: &[u64], head: &[u8], len: u64) -> Option<u64> {
    Some(aqr_api::stack::whole(params, &[], inputs) + arsc::decode_plan(head, len)?)
}

/// The module: the file of `len` bytes from `params` (none) and `inputs` ([`arsc::INPUTS`]).
pub fn decode(params: &[u8], inputs: &[&[u8]], len: u64) -> Result<Vec<u8>> {
    if !params.is_empty() {
        return Err(corrupt());
    }
    arsc::decode(inputs, usize::try_from(len).map_err(|_| corrupt())?)
}
