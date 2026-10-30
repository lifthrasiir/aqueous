//! The code of the module `gif-recons` (see `formula`, and `FORMAT.md`), which needs no
//! aqr-formula, so that aqr-modules builds it into its Wasm module.

use crate::{Error, Result, corrupt, recons};

/// The module: the file of `len` bytes that the inputs (skeleton, record) rebuild (see
/// `FORMAT.md`).
pub fn decode(params: &[u8], inputs: &[&[u8]], len: u64, _threads: usize) -> Result<Vec<u8>> {
    let &[skeleton, record] = inputs else { return Err(Error("two inputs wanted")) };
    if !params.is_empty() {
        return Err(corrupt());
    }
    let len = usize::try_from(len).map_err(|_| corrupt())?;
    let mut out = Vec::with_capacity(len);
    recons::join_onto(skeleton, record, &mut out)?;
    if out.len() != len {
        return Err(Error("output of another length"));
    }
    Ok(out)
}
