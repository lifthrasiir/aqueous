//! The code of the module `xpress-recons` (see `formula`, and `FORMAT.md`), which needs no
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

/// The module: of the inputs (data, records, bytes kept, index), each stream of XPRESS Huffman
/// chunks that its data and record rebuild after the bytes kept before it, then the bytes kept
/// after the last, `len` bytes in all (see `FORMAT.md`). Streams are rebuilt side by side on up to
/// `threads` threads.
pub fn decode(params: &[u8], inputs: &[&[u8]], len: u64, threads: usize) -> Result<Vec<u8>> {
    let &[data, records, kept, index] = inputs else { return Err(Error("four inputs wanted")) };
    if !params.is_empty() {
        return Err(corrupt());
    }
    let (streams, tail) = cut(data, records, kept, index)?;
    let len = usize::try_from(len).map_err(|_| corrupt())?;
    // (a little room past the length: the last token of a damaged stream is stopped by the check
    // of each token, not by the vector's end)
    let mut out = Vec::with_capacity(len + recons::SLACK);
    if threads <= 1 || streams.len() <= 1 || cfg!(target_family = "wasm") {
        // (the streams one after another on the match finder made once)
        let mut spare = recons::Spare::new();
        for &(kept, data, record) in &streams {
            out.extend_from_slice(kept);
            let (s, done) = recons::join_in(spare, data, record, &mut out, len);
            spare = s;
            done?;
            if out.len() > len {
                return Err(Error("output of another length"));
            }
        }
    } else {
        let join = |i: usize| {
            let mut out = Vec::new();
            recons::join_onto(streams[i].1, streams[i].2, &mut out, len).map(|_| out)
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
