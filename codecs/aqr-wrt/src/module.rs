//! The code of the modules (see `formula`, and `FORMAT.md`), which needs no aqr-formula, so
//! that aqr-modules builds it into their Wasm modules.

use crate::words::{self};
use crate::{Result, corrupt, hangul};

/// `wrt-recons`: the text of `len` bytes from `resources` (the dictionary) and `inputs` (the
/// coded text), in the pieces of `params`, on up to `threads` threads.
pub fn decode(params: &[u8], resources: &[&[u8]], inputs: &[&[u8]], len: u64, threads: usize) -> Result<Vec<u8>> {
    let (&[dict], &[text]) = (resources, inputs) else { return Err(corrupt()) };
    let len = usize::try_from(len).map_err(|_| corrupt())?;
    words::decode(params, dict, text, len, threads)
}

/// `wrt-recons-hangul`: the text of `len` bytes from `resources` (the dictionary) and `inputs`
/// (the coded text), in the pieces of `params`, on up to `threads` threads.
pub fn decode_hangul(
    params: &[u8],
    resources: &[&[u8]],
    inputs: &[&[u8]],
    len: u64,
    threads: usize,
) -> Result<Vec<u8>> {
    let (&[dict], &[text]) = (resources, inputs) else { return Err(corrupt()) };
    let len = usize::try_from(len).map_err(|_| corrupt())?;
    hangul::decode(params, dict, text, len, threads)
}
