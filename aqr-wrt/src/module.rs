//! The code of the module (see `formula`, and `FORMAT.md`), which needs no aqr-formula, so that
//! aqr-modules builds it into its Wasm module.

use crate::words::{self};
use crate::{Result, corrupt};

/// The module: the text of `len` bytes from `resources` (the dictionary) and `inputs` (the
/// coded text); no parameters.
pub fn decode(params: &[u8], resources: &[&[u8]], inputs: &[&[u8]], len: u64, _threads: usize) -> Result<Vec<u8>> {
    let (&[], &[dict], &[text]) = (params, resources, inputs) else { return Err(corrupt()) };
    let len = usize::try_from(len).map_err(|_| corrupt())?;
    words::decode(dict, text, len)
}
