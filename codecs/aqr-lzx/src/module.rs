//! The code of the module `lzx-recons` (see `formula`, and `FORMAT.md`), which needs no
//! aqr-formula, so that aqr-modules builds it into its Wasm module.

use aqr_common::varint;

use crate::lzx::{self, Variant};
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

/// The parts of `index`.
pub fn parts(index: &[u8]) -> Result<Vec<Part>> {
    let mut at = 0;
    let mut out = Vec::new();
    while at < index.len() {
        let mut get = || varint::get(index, &mut at).ok().and_then(|n| usize::try_from(n).ok()).ok_or_else(corrupt);
        out.push(Part { kept: get()?, data: get()?, record: get()? });
    }
    Ok(out)
}

/// What `index` cuts the inputs into: for each stream, the bytes kept before it, its data and
/// its record; and the bytes kept after the last.
#[allow(clippy::type_complexity)]
pub(crate) fn cut<'a>(
    data: &'a [u8],
    records: &'a [u8],
    kept: &'a [u8],
    index: &[u8],
) -> Result<(Vec<(&'a [u8], &'a [u8], &'a [u8])>, &'a [u8])> {
    let (lens, _) = cut_lens(index, [kept.len(), data.len(), records.len()])?;
    let (mut k, mut d, mut r) = (kept, data, records);
    let mut out = Vec::with_capacity(lens.len());
    for [nk, nd, nr] in lens {
        let take = |from: &mut &'a [u8], n: usize| {
            let (a, b) = from.split_at(n);
            *from = b;
            a
        };
        out.push((take(&mut k, nk), take(&mut d, nd), take(&mut r, nr)));
    }
    Ok((out, k))
}

/// [`cut`] by lengths alone: of the inputs `lens` (bytes kept, data, records) long, the lengths
/// of each stream's bytes kept, data and record; and of the bytes kept after the last. A plan
/// takes this, as the data may not be at hand when it is made.
pub(crate) fn cut_lens(index: &[u8], lens: [usize; 3]) -> Result<(Vec<[usize; 3]>, usize)> {
    let (mut at, mut left) = (0, lens);
    let mut out = Vec::new();
    while at < index.len() {
        let mut one = [0; 3];
        // (the order of the index: kept, data, record; of the inputs': kept, data, record)
        for (n, left) in one.iter_mut().zip(&mut left) {
            let v = varint::get(index, &mut at).map_err(|_| corrupt())?;
            *n = usize::try_from(v).ok().filter(|&v| v <= *left).ok_or_else(corrupt)?;
            *left -= *n;
        }
        out.push(one);
    }
    if left[1] != 0 || left[2] != 0 {
        return Err(corrupt());
    }
    Ok((out, left[0]))
}

/// The variant of the streams that `params` tell: none for WIM's chunks, the window's bits for
/// CAB's streams.
pub fn variant(params: &[u8]) -> Result<Variant> {
    match *params {
        [] => Ok(lzx::WIM),
        [bits] => Variant::cab(bits).ok_or_else(corrupt),
        _ => Err(corrupt()),
    }
}

/// The bytes of data a chunk of the streams of `v` comes to at most, of those of `lens` bytes of
/// data: a CAB stream is one, WIM's chunks are of [`recons::CHUNK`].
pub fn spare_data(v: Variant, lens: impl Iterator<Item = usize>) -> usize {
    let most = lens.max().unwrap_or(0);
    if v.cab { most } else { most.min(recons::CHUNK) }
}

/// The module: of the inputs (data, records, bytes kept, index), each stream of LZX chunks (or,
/// with `params` the window's bits, each CAB stream) that its data and record rebuild after the
/// bytes kept before it, then the bytes kept after the last, `len` bytes in all (see
/// `FORMAT.md`). Streams are rebuilt side by side on up to `threads` threads.
pub fn decode(params: &[u8], inputs: &[&[u8]], len: u64, threads: usize) -> Result<Vec<u8>> {
    let &[data, records, kept, index] = inputs else { return Err(Error("four inputs wanted")) };
    let v = variant(params)?;
    let join_onto = |data: &[u8], record: &[u8], out: &mut Vec<u8>| match v.cab {
        true => recons::join_stream_onto(data, record, v, out),
        false => recons::join_onto(data, record, out),
    };
    let (streams, tail) = cut(data, records, kept, index)?;
    let len = usize::try_from(len).map_err(|_| corrupt())?;
    let mut out = Vec::with_capacity(len);
    if threads <= 1 || streams.len() <= 1 || cfg!(target_family = "wasm") {
        // (the streams one after another on the match finder and chunk made once, for the largest)
        let mut spare = recons::Spare::new(v.window(), spare_data(v, streams.iter().map(|s| s.1.len())));
        for &(kept, data, record) in &streams {
            out.extend_from_slice(kept);
            let (s, done) = recons::join_in(spare, v, data, record, &mut out);
            spare = s;
            done?;
            if out.len() > len {
                return Err(Error("output of another length"));
            }
        }
    } else {
        let join = |i: usize| {
            let mut out = Vec::new();
            join_onto(streams[i].1, streams[i].2, &mut out).map(|_| out)
        };
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
