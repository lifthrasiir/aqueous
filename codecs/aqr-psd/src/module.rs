//! The code of the module (see `formula`, and FORMAT.md), which needs no aqr-formula, so that
//! aqr-modules builds it into its Wasm module.

use crate::codec;
use crate::psd::{Error, Result};

/// The module: the file of `len` bytes rebuilt from `inputs`, the head, the skeleton (the
/// bytes outside the spans), the embedded files (their bytes one after another, in file
/// order) and the model stream. `threads` is the number of threads to decode with (the same bytes whatever
/// the number).
pub fn decode(params: &[u8], inputs: &[&[u8]], len: u64, threads: usize) -> Result<Vec<u8>> {
    let (&[], &[head, skeleton, embedded, model]) = (params, inputs) else {
        return Err(Error("bad parameters or inputs".into()));
    };
    codec::decode_formula(head, skeleton, embedded, model, len, threads)
}
