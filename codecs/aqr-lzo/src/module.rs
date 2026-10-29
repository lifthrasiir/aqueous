//! The code of the module `lzo-recons` (see `formula`, and `FORMAT.md`), which needs no
//! aqr-formula, so that aqr-modules builds it into its Wasm module.

use aqr_common::varint;

use crate::{Error, Result, corrupt, recons};

/// The index of streams of data and records of `lens` bytes: for each, the two lengths as
/// varints; empty for a single stream.
pub fn index(lens: &[(usize, usize)]) -> Vec<u8> {
    let mut out = Vec::new();
    if lens.len() > 1 {
        for &(d, r) in lens {
            varint::put(&mut out, d as u64);
            varint::put(&mut out, r as u64);
        }
    }
    out
}

/// The data and records of each stream that `index` cuts `data` and `records` into.
fn cut<'a>(data: &'a [u8], records: &'a [u8], index: &[u8]) -> Result<Vec<(&'a [u8], &'a [u8])>> {
    if index.is_empty() {
        return Ok(vec![(data, records)]);
    }
    let (mut at, mut d, mut r) = (0, data, records);
    let mut out = Vec::new();
    while at < index.len() {
        let mut take = |from: &mut &'a [u8]| {
            let n = varint::get(index, &mut at).map_err(|_| corrupt())?;
            let n = usize::try_from(n).ok().filter(|&n| n <= from.len()).ok_or_else(corrupt)?;
            let (a, b) = from.split_at(n);
            *from = b;
            Ok::<_, Error>(a)
        };
        out.push((take(&mut d)?, take(&mut r)?));
    }
    if !d.is_empty() || !r.is_empty() {
        return Err(corrupt());
    }
    Ok(out)
}

/// The module: the LZO1X streams of `len` bytes in all that the data and records of the inputs
/// (data, records, index) rebuild, one after another (see `FORMAT.md`). Streams are rebuilt
/// side by side on up to `threads` threads.
pub fn decode(params: &[u8], inputs: &[&[u8]], len: u64, threads: usize) -> Result<Vec<u8>> {
    let &[data, records, index] = inputs else { return Err(Error("three inputs wanted")) };
    if !params.is_empty() {
        return Err(corrupt());
    }
    let streams = cut(data, records, index)?;
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
