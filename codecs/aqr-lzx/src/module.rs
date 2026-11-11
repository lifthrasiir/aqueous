//! The code of the module `lzx-recons` (see `formula`, and `FORMAT.md`), which needs no
//! aqr-formula, so that aqr-modules builds it into its Wasm module.

use aqr_api::Memory;
use aqr_api::stream::{In, Pull, Put, Reader, Slice, Written};
use aqr_common::varint;
use aqr_lzma::stream::PIECE;

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
        let mut spare = recons::Spare::new(v.window(), 0);
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

/// Bytes of data from which an application is better streamed than gathered whole: a stream that
/// long can not be spread over threads, and holding it (and its output) takes more memory than
/// the time of the threads is worth.
pub const STREAMED_MIN: u64 = 256 << 20;

/// Whether an application of inputs of `lens` bytes (data, records, bytes kept, index) is of
/// such streams: of few (at most some, as the index tells), their data long ([`STREAMED_MIN`]).
/// An application of many streams (a WIM's) is spread over threads whole.
pub fn streamed(lens: &[u64]) -> bool {
    matches!(lens, &[data, _, _, index] if data >= STREAMED_MIN && index <= 64)
}

/// Pages of the windows of the module as a part module ([`decode_part`]): of each input and of
/// the output.
pub const WINDOW: u32 = 16;

/// Bytes of the pieces in which the index is read.
pub const INDEX_PIECE: usize = 256;

/// The next `left` bytes of a stream, no more.
struct Limit<'a> {
    src: &'a mut dyn Pull,
    left: usize,
}

impl Pull for Limit<'_> {
    fn pull(&mut self, buf: &mut [u8]) -> usize {
        let k = self.left.min(buf.len());
        let n = if k == 0 { 0 } else { self.src.pull(&mut buf[..k]) };
        self.left -= n;
        n
    }
}

/// The module as a part module on the inputs `data`, `rec` (records), `kept` and `index` of
/// `lens` bytes: [`decode`] with its inputs taken and its output put in order through windows,
/// in pieces. It holds the window and the block being coded of a CAB stream, never the stream.
fn run(v: Variant, [data, rec, kept, index]: [&mut dyn Pull; 4], lens: [u64; 4], out: &mut dyn Put) -> Result<()> {
    let [mut data_left, mut rec_left, mut kept_left, _] = lens;
    let mut index = Reader::new(index, INDEX_PIECE);
    let mut rec = Reader::new(rec, PIECE);
    let mut kept = Reader::new(kept, PIECE);
    let mut spare = recons::Spare::new(v.window(), 0);
    let mut put = |d: &[u8]| out.put(d).ok_or(Error("output of another length"));
    while !index.done() {
        let mut take = |left: &mut u64| -> Result<u64> {
            let n = index.num().map_err(|_| corrupt())?;
            *left = left.checked_sub(n).ok_or_else(corrupt)?;
            Ok(n)
        };
        let (k, n, r) = (take(&mut kept_left)?, take(&mut data_left)?, take(&mut rec_left)?);
        kept.each(k, |b| put(b).ok()).ok_or_else(corrupt)?;
        let n = usize::try_from(n).map_err(|_| corrupt())?;
        let mut sink = Sink(&mut put);
        let (s, done) =
            recons::join_stream(spare, v, &mut Limit { src: &mut *data, left: n }, n, &mut rec, r, &mut sink);
        spare = s;
        done?;
    }
    kept.each(kept_left, |b| put(b).ok()).ok_or_else(corrupt)?;
    match data_left == 0 && rec_left == 0 && rec.done() && kept.done() {
        true => Ok(()),
        false => Err(corrupt()),
    }
}

/// A closure as a [`Put`].
struct Sink<'a, F: FnMut(&[u8]) -> Result<()>>(&'a mut F);

impl<F: FnMut(&[u8]) -> Result<()>> Put for Sink<'_, F> {
    fn put(&mut self, d: &[u8]) -> Option<()> {
        (self.0)(d).ok()
    }
}

/// The module `lzx-recons` as a part module (its entry point, of `aqr_api::entry!`): the inputs
/// are data, records, bytes kept and the index, of `lens` bytes, the output `size` bytes.
pub fn decode_part(params_len: u64, lens: [u64; 4], size: u64) {
    aqr_api::finish(part(params_len, lens, size));
}

fn part(params_len: u64, lens: [u64; 4], size: u64) -> Result<()> {
    let mut params = [0u8; 1];
    let Some(buf) = params.get_mut(..params_len as usize) else { return Err(corrupt()) };
    aqr_api::Params::read(0, buf);
    let v = variant(buf)?;
    let (mut i0, mut i1, mut i2, mut i3) =
        (In::<0>::new(lens[0]), In::<1>::new(lens[1]), In::<2>::new(lens[2]), In::<3>::new(lens[3]));
    let mut out = Written::new(size);
    run(v, [&mut i0, &mut i1, &mut i2, &mut i3], lens, &mut out)?;
    match out.total() == size {
        true => Ok(()),
        false => Err(Error("output of another length")),
    }
}

/// [`decode_part`] on whole inputs, natively: what a part module gives, one stream after another.
pub fn decode_streamed(params: &[u8], inputs: &[&[u8]], len: u64) -> Result<Vec<u8>> {
    let &[data, records, kept, index] = inputs else { return Err(Error("four inputs wanted")) };
    let v = variant(params)?;
    let lens = [data.len() as u64, records.len() as u64, kept.len() as u64, index.len() as u64];
    let mut out = Vec::new();
    run(v, [&mut Slice(data), &mut Slice(records), &mut Slice(kept), &mut Slice(index)], lens, &mut out)?;
    match out.len() as u64 == len {
        true => Ok(out),
        false => Err(Error("output of another length")),
    }
}
