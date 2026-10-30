//! The code of the module (see `formula`, and `FORMAT.md`), which needs no aqr-formula, so that
//! aqr-modules builds it into its Wasm module.

use crate::{Result, corrupt, layout};

/// The module: the archive of `len` bytes rebuilt from `inputs`, the data, the header and the
/// index.
pub fn decode(params: &[u8], inputs: &[&[u8]], len: u64, _threads: usize) -> Result<Vec<u8>> {
    let (&[], &[data, header, index]) = (params, inputs) else { return Err(corrupt()) };
    layout::build(data, header, index, len)
}
