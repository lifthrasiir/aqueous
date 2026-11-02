//! The code of the module `lzo-recons` (see `formula`, and `FORMAT.md`), which needs no
//! aqr-formula, so that aqr-modules builds it into its Wasm module.

use crate::{Error, Result, corrupt, recons};

pub use aqr_common::streams::index;

/// The module: the LZO1X streams of `len` bytes in all that the data and records of the inputs
/// (data, records, index) rebuild, one after another (see `FORMAT.md`). Streams are rebuilt
/// side by side on up to `threads` threads.
pub fn decode(params: &[u8], inputs: &[&[u8]], len: u64, threads: usize) -> Result<Vec<u8>> {
    let &[data, records, index] = inputs else { return Err(Error("three inputs wanted")) };
    if !params.is_empty() {
        return Err(corrupt());
    }
    let streams = aqr_common::streams::cut(data, records, index, corrupt)?;
    let join = |i: usize| recons::join(streams[i].0, streams[i].1);
    let len = usize::try_from(len).map_err(|_| corrupt())?;
    let mut out = Vec::with_capacity(len);
    if threads <= 1 || cfg!(target_family = "wasm") {
        // (each written onto the output as its parse is decoded: no stream nor parse held)
        for &(data, record) in &streams {
            recons::join_onto(data, record, &mut out)?;
        }
    } else {
        for o in aqr_common::pool::par_map(streams.len(), threads, join) {
            out.extend_from_slice(&o?);
        }
    }
    if out.len() != len {
        return Err(Error("output of another length"));
    }
    Ok(out)
}
