//! The code of the modules `rar5-recons` and `rar3-recons` (see `formula`, and `FORMAT.md`),
//! which needs no aqr-formula, so that aqr-modules builds it into its Wasm modules.

use aqr_common::varint;

use crate::{Error, Result, corrupt};

pub use aqr_common::streams::index;

/// What joins a stream's data and record onto an output: the sizes of its files' parts.
type Join<'a> = &'a (dyn Fn(&[u8], &[u8], &mut Vec<u8>) -> Result<()> + Sync);

/// A module: the streams of `len` bytes in all that the data and records of the inputs (data,
/// records, index) rebuild with `join`, one after another (see `FORMAT.md`). Streams are rebuilt
/// side by side on up to `threads` threads natively; one after another in Wasm.
fn decode_with(inputs: &[&[u8]], len: u64, threads: usize, join: Join) -> Result<Vec<u8>> {
    let &[data, records, index] = inputs else { return Err(Error("three inputs wanted")) };
    let streams = aqr_common::streams::cut(data, records, index, corrupt)?;
    let len = usize::try_from(len).map_err(|_| corrupt())?;
    let mut out = Vec::with_capacity(len);
    if threads <= 1 || streams.len() <= 1 || cfg!(target_family = "wasm") {
        for &(data, record) in &streams {
            join(data, record, &mut out)?;
            if out.len() > len {
                return Err(Error("output of another length"));
            }
        }
    } else {
        let made = aqr_common::pool::par_map(streams.len(), threads, |i| {
            let mut v = Vec::new();
            join(streams[i].0, streams[i].1, &mut v).map(|_| v)
        });
        for s in made {
            out.extend_from_slice(&s?);
        }
    }
    if out.len() != len {
        return Err(Error("output of another length"));
    }
    Ok(out)
}

/// The module `rar5-recons`: RAR5 streams. No params.
pub fn decode5(params: &[u8], inputs: &[&[u8]], len: u64, threads: usize) -> Result<Vec<u8>> {
    if !params.is_empty() {
        return Err(corrupt());
    }
    decode_with(inputs, len, threads, &crate::recons5::join_onto)
}

/// The params of `rar3-recons`: the largest PPMd model of its streams in MiB, 0 for none.
pub fn params3(ppmd: u32) -> Vec<u8> {
    let mut out = Vec::new();
    varint::put(&mut out, ppmd as u64);
    out
}

/// The largest PPMd model of `rar3-recons`'s params, in MiB.
pub fn ppmd_of(params: &[u8]) -> Result<u32> {
    let mut at = 0;
    let v = varint::get(params, &mut at).map_err(|_| corrupt())?;
    if at != params.len() || v > 256 {
        return Err(corrupt());
    }
    Ok(v as u32)
}

/// The module `rar3-recons`: RAR 2.9 streams (RAR 3.x and 4.x), their PPMd models within the
/// params' size.
pub fn decode3(params: &[u8], inputs: &[&[u8]], len: u64, threads: usize) -> Result<Vec<u8>> {
    let limit = ppmd_of(params)?;
    decode_with(inputs, len, threads, &move |d, r, o| crate::recons3::join_onto(d, r, o, limit))
}
