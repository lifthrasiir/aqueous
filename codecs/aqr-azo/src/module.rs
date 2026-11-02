//! The code of the module `azo-recons` (see `formula`, and `FORMAT.md`), which needs no
//! aqr-formula, so that aqr-modules builds it into its Wasm module.

use crate::{Error, Result, corrupt, recons};

pub use aqr_common::streams::index;

/// The module: the AZO streams of `len` bytes in all that the data and records of the inputs
/// (data, records, index) rebuild, one after another (see `FORMAT.md`). Streams are rebuilt
/// side by side on up to `threads` threads.
pub fn decode(params: &[u8], inputs: &[&[u8]], len: u64, threads: usize) -> Result<Vec<u8>> {
    let &[data, records, index] = inputs else { return Err(Error("three inputs wanted")) };
    if !params.is_empty() {
        return Err(corrupt());
    }
    let streams = aqr_common::streams::cut(data, records, index, corrupt)?;
    let len = usize::try_from(len).map_err(|_| corrupt())?;
    let mut out = Vec::with_capacity(len);
    // (each written onto the output in turn, its blocks side by side natively: a stream's blocks
    // are its work, of 512 KiB each)
    for &(data, record) in &streams {
        recons::join_onto(data, record, &mut out, threads)?;
    }
    if out.len() != len {
        return Err(Error("output of another length"));
    }
    Ok(out)
}
