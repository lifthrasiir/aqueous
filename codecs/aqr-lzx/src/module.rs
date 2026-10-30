//! The code of the module `lzx-recons` (see `formula`, and `FORMAT.md`), which needs no
//! aqr-formula, so that aqr-modules builds it into its Wasm module.

use aqr_common::varint;

use crate::{Error, Result, corrupt, recons};

/// A stream of the output: the bytes kept before it, and the bytes of its data and of its
/// record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Part {
    pub kept: usize,
    pub data: usize,
    pub record: usize,
}

/// The index of `parts`: for each, its three lengths as varints.
pub fn index(parts: &[Part]) -> Vec<u8> {
    let mut out = Vec::new();
    for p in parts {
        varint::put(&mut out, p.kept as u64);
        varint::put(&mut out, p.data as u64);
        varint::put(&mut out, p.record as u64);
    }
    out
}

/// What `index` cuts the inputs into: for each stream, the bytes kept before it, its data and
/// its record; and the bytes kept after the last.
#[allow(clippy::type_complexity)]
fn cut<'a>(
    data: &'a [u8],
    records: &'a [u8],
    kept: &'a [u8],
    index: &[u8],
) -> Result<(Vec<(&'a [u8], &'a [u8], &'a [u8])>, &'a [u8])> {
    let (mut at, mut d, mut r, mut k) = (0, data, records, kept);
    let mut out = Vec::new();
    while at < index.len() {
        let mut take = |from: &mut &'a [u8]| {
            let n = varint::get(index, &mut at).map_err(|_| corrupt())?;
            let n = usize::try_from(n).ok().filter(|&n| n <= from.len()).ok_or_else(corrupt)?;
            let (a, b) = from.split_at(n);
            *from = b;
            Ok::<_, Error>(a)
        };
        out.push((take(&mut k)?, take(&mut d)?, take(&mut r)?));
    }
    if !d.is_empty() || !r.is_empty() {
        return Err(corrupt());
    }
    Ok((out, k))
}

/// The module: of the inputs (data, records, bytes kept, index), each stream of LZX chunks that
/// its data and record rebuild after the bytes kept before it, then the bytes kept after the
/// last, `len` bytes in all (see `FORMAT.md`). Streams are rebuilt side by side on up to
/// `threads` threads.
pub fn decode(params: &[u8], inputs: &[&[u8]], len: u64, threads: usize) -> Result<Vec<u8>> {
    let &[data, records, kept, index] = inputs else { return Err(Error("four inputs wanted")) };
    if !params.is_empty() {
        return Err(corrupt());
    }
    let (streams, tail) = cut(data, records, kept, index)?;
    let len = usize::try_from(len).map_err(|_| corrupt())?;
    let mut out = Vec::with_capacity(len);
    if threads <= 1 || streams.len() <= 1 || cfg!(target_family = "wasm") {
        for &(kept, data, record) in &streams {
            out.extend_from_slice(kept);
            recons::join_onto(data, record, &mut out)?;
            if out.len() > len {
                return Err(Error("output of another length"));
            }
        }
    } else {
        let join = |i: usize| recons::join(streams[i].1, streams[i].2);
        for (o, s) in aqr_common::pool::par_map(streams.len(), threads, join).into_iter().zip(&streams) {
            out.extend_from_slice(s.0);
            out.extend_from_slice(&o?);
        }
    }
    out.extend_from_slice(tail);
    if out.len() != len {
        return Err(Error("output of another length"));
    }
    Ok(out)
}
