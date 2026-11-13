//! The code of the modules `lzma-recons` and `lzma2-recons` (see `formula`, and `FORMAT.md`),
//! which needs no aqr-formula, so that aqr-modules builds them into their Wasm modules.

use crate::stream::{PIECE, Pull, Put, Reader};
use crate::{Error, Result, corrupt, lzma2, recons};
use aqr_api::stream::{In, Written};

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
    let mut out = Vec::new();
    out.try_reserve_exact(len).map_err(|_| corrupt())?;
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

/// Pages of the windows of the modules as part modules ([`decode_part`]): of each input and of the
/// output.
pub const WINDOW: u32 = 16;

/// Bytes of the pieces in which the index is read.
pub const INDEX_PIECE: usize = 256;

/// What a part module does for a stream: its data (`n` bytes, read from the pull), its record
/// and what it writes to the sink.
type Stream = fn(&mut dyn Pull, usize, &mut Reader, u64, &mut dyn Put) -> Result<()>;

/// The module as a part module on streams of `lens` bytes of data and records and the index,
/// its output `size` bytes: [`decode`] or [`decode2`] (by `stream`) with its inputs taken and
/// its output put in order through windows, in pieces. Nothing is held of the lengths, but the
/// room that a stream's match finder and window take, which a dictionary decides.
fn run(
    data: &mut dyn Pull,
    rec: &mut dyn Pull,
    index: &mut dyn Pull,
    lens: [u64; 3],
    out: &mut dyn Put,
    stream: Stream,
) -> Result<()> {
    let [data_len, rec_len, index_len] = lens;
    let mut index = Reader::new(index, INDEX_PIECE);
    let mut rec = Reader::new(rec, PIECE);
    let (mut data_left, mut rec_left) = (data_len, rec_len);
    // (a single stream has no index)
    if index_len == 0 {
        stream(data, usize::try_from(data_len).map_err(|_| corrupt())?, &mut rec, rec_len, out)?;
        (data_left, rec_left) = (0, 0);
    }
    while !index.done() {
        let n = index.num()?;
        let r = index.num()?;
        if n > data_left || r > rec_left {
            return Err(corrupt());
        }
        (data_left, rec_left) = (data_left - n, rec_left - r);
        stream(data, usize::try_from(n).map_err(|_| corrupt())?, &mut rec, r, out)?;
    }
    match data_left == 0 && rec_left == 0 && rec.done() {
        true => Ok(()),
        false => Err(corrupt()),
    }
}

/// The module `lzma-recons` as a part module (its entry point, of `aqr_api::entry!`).
pub fn decode_part(params_len: u64, lens: [u64; 3], size: u64) {
    aqr_api::finish(part(params_len, lens, size, recons::join_stream));
}

/// The module `lzma2-recons` as a part module (its entry point, of `aqr_api::entry!`).
pub fn decode2_part(params_len: u64, lens: [u64; 3], size: u64) {
    aqr_api::finish(part(params_len, lens, size, lzma2::join_stream));
}

fn part(params_len: u64, lens: [u64; 3], size: u64, stream: Stream) -> Result<()> {
    if params_len != 0 {
        return Err(corrupt());
    }
    let (mut i0, mut i1, mut i2) = (In::<0>::new(lens[0]), In::<1>::new(lens[1]), In::<2>::new(lens[2]));
    let mut out = Written::new(size);
    run(&mut i0, &mut i1, &mut i2, lens, &mut out, stream)?;
    match out.total() == size {
        true => Ok(()),
        false => Err(Error("output of another length")),
    }
}

/// [`decode_part`] (or [`decode2_part`], where `lzma2`) on whole inputs, natively: what a part
/// module gives, one stream after another.
pub fn decode_streamed(params: &[u8], inputs: &[&[u8]], len: u64, lzma2: bool) -> Result<Vec<u8>> {
    use crate::stream::Slice;
    let &[data, records, index] = inputs else { return Err(Error("three inputs wanted")) };
    if !params.is_empty() {
        return Err(corrupt());
    }
    let lens = [data.len() as u64, records.len() as u64, index.len() as u64];
    let mut out = Vec::new();
    let stream: Stream = if lzma2 { lzma2::join_stream } else { recons::join_stream };
    run(&mut Slice(data), &mut Slice(records), &mut Slice(index), lens, &mut out, stream)?;
    match out.len() as u64 == len {
        true => Ok(out),
        false => Err(Error("output of another length")),
    }
}
