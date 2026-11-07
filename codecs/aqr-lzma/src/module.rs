//! The code of the modules `lzma-recons` and `lzma2-recons` (see `formula`, and `FORMAT.md`),
//! which needs no aqr-formula, so that aqr-modules builds them into their Wasm modules.

use crate::{Error, Result, corrupt, lzma2, recons};

pub use aqr_common::streams::index;

/// The module `lzma-recons`: the LZMA streams of `len` bytes in all that the data and records of
/// the inputs (data, records, index) rebuild, one after another (see `FORMAT.md`). Streams are
/// rebuilt side by side on up to `threads` threads natively (a stream's parts on them where it is
/// alone); one after another, written onto the output as they are made, in Wasm.
pub fn decode(params: &[u8], inputs: &[&[u8]], len: u64, threads: usize) -> Result<Vec<u8>> {
    decode_with(params, inputs, len, threads, recons::join_onto)
}

/// The module `lzma2-recons`: [`decode`] for the segments of LZMA2 streams.
pub fn decode2(params: &[u8], inputs: &[&[u8]], len: u64, threads: usize) -> Result<Vec<u8>> {
    decode_with(params, inputs, len, threads, |d, r, _, out| lzma2::join_onto(d, r, out))
}

/// What rebuilds a stream from its data and its record (and the threads for its parts), onto
/// the end of a vector.
type Join = fn(&[u8], &[u8], usize, &mut Vec<u8>) -> Option<()>;

fn decode_with(params: &[u8], inputs: &[&[u8]], len: u64, threads: usize, join: Join) -> Result<Vec<u8>> {
    let &[data, records, index] = inputs else { return Err(Error("three inputs wanted")) };
    if !params.is_empty() {
        return Err(corrupt());
    }
    let streams = aqr_common::streams::cut(data, records, index, corrupt)?;
    let len = usize::try_from(len).map_err(|_| corrupt())?;
    let mut out = Vec::with_capacity(len);
    if threads <= 1 || streams.len() <= 1 || cfg!(target_family = "wasm") {
        // (a stream's parts on the threads of all)
        for &(data, record) in &streams {
            join(data, record, threads, &mut out).ok_or_else(corrupt)?;
            if out.len() > len {
                return Err(Error("output of another length"));
            }
        }
    } else {
        let made = aqr_common::pool::par_map(streams.len(), threads, |i| {
            let (data, record) = streams[i];
            let mut o = Vec::with_capacity(data.len() / 4);
            join(data, record, threads, &mut o).map(|_| o).ok_or_else(corrupt)
        });
        for o in made {
            out.extend_from_slice(&o?);
        }
    }
    if out.len() != len {
        return Err(Error("output of another length"));
    }
    Ok(out)
}
