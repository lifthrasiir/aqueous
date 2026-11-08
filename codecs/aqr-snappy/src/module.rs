//! The code of the modules (see `formula`, and `FORMAT.md`), which needs no aqr-formula, so that
//! aqr-modules builds it into their Wasm modules.

use aqr_api::stream::{In, Pull, Put, Slice, Written};

use crate::snappy::{self, FRAGMENT, Hash};
use crate::{Error, Result, corrupt, leveldb};

/// Pages of the windows of `snappy-recons` as a part module ([`decode_part`]): of the input and
/// of the output.
pub const WINDOW: u32 = 4;

/// The module `snappy-recons`: a Snappy stream of `len` bytes (see `FORMAT.md`).
pub fn decode(params: &[u8], inputs: &[&[u8]], len: u64, _threads: usize) -> Result<Vec<u8>> {
    let (&[h], &[data]) = (params, inputs) else { return Err(corrupt()) };
    let hash = Hash::from_u8(h).ok_or_else(corrupt)?;
    of_len(snappy::compress_in(data, hash, cap(len))?, len)
}

/// The memory plan of `snappy-recons` as a part module ([`aqr_api::stack`]): what [`decode_part`]
/// takes beyond its initial memory (`initial` bytes) at most, of an input of `input` bytes: a
/// fragment of the data, the table, and the fragment's stream, whatever the lengths.
pub fn plan(initial: u64, input: u64) -> u64 {
    let f = input.min(FRAGMENT as u64);
    let mut m = aqr_api::stack::Sim::new(initial);
    m.take(f);
    m.take(2 * snappy::table_size(f as usize) as u64);
    m.take(fragment_bound(f));
    m.plan()
}

/// Bytes a stream of a fragment of `n` bytes takes at most (`MaxCompressedLength`).
fn fragment_bound(n: u64) -> u64 {
    32 + n + n / 6
}

/// The Snappy stream of the `n` bytes pulled from `src`, put to `out` a fragment at a time (the
/// length of the stream is for the caller to check).
pub fn compress_stream(src: &mut dyn Pull, n: u64, hash: Hash, out: &mut dyn Put) -> Result<()> {
    let n32 = u32::try_from(n).map_err(|_| Error("input too long for Snappy"))?;
    // (the length as a varint)
    let (mut head, mut k, mut v) = ([0u8; 5], 0, n32);
    loop {
        head[k] = (v & 0x7f) as u8 | if v >= 0x80 { 0x80 } else { 0 };
        k += 1;
        v >>= 7;
        if v == 0 {
            break;
        }
    }
    let n = n as usize;
    let f = n.min(FRAGMENT);
    let mut data = vec![0; f];
    let mut table = vec![0u16; snappy::table_size(f)];
    let mut stream = Vec::with_capacity(fragment_bound(f as u64) as usize);
    let put = |out: &mut dyn Put, d: &[u8]| out.put(d).ok_or(Error("output of another length"));
    put(out, &head[..k])?;
    let mut left = n;
    while left > 0 {
        let piece = &mut data[..left.min(FRAGMENT)];
        let mut at = 0;
        while at < piece.len() {
            match src.pull(&mut piece[at..]) {
                0 => return Err(Error("data of another length")),
                k => at += k,
            }
        }
        left -= piece.len();
        stream.clear();
        match hash {
            Hash::Mul => snappy::fragment::<false>(piece, &mut table, &mut stream),
            Hash::Crc => snappy::fragment::<true>(piece, &mut table, &mut stream),
        }
        put(out, &stream)?;
    }
    Ok(())
}

/// The module `snappy-recons` as a part module (its entry point, of `aqr_api::entry!`): what
/// [`decode`] gives, its input taken and its output put in order through windows ([`WINDOW`]),
/// a fragment at a time.
pub fn decode_part(params_len: u64, in_len: u64, size: u64) {
    aqr_api::finish(part(params_len, in_len, size));
}

fn part(params_len: u64, in_len: u64, size: u64) -> Result<()> {
    use aqr_api::Memory;
    let mut p = [0; 1];
    if params_len != 1 {
        return Err(corrupt());
    }
    aqr_api::Params::read(0, &mut p);
    let hash = Hash::from_u8(p[0]).ok_or_else(corrupt)?;
    let mut data = In::<0>::new(in_len);
    let mut out = Written::new(size);
    compress_stream(&mut data, in_len, hash, &mut out)?;
    match out.total() == size {
        true => Ok(()),
        false => Err(Error("output of another length")),
    }
}

/// [`decode_part`] on a whole input, natively.
pub fn decode_streamed(params: &[u8], inputs: &[&[u8]], len: u64) -> Result<Vec<u8>> {
    let (&[h], &[data]) = (params, inputs) else { return Err(corrupt()) };
    let hash = Hash::from_u8(h).ok_or_else(corrupt)?;
    let mut out = Vec::new();
    compress_stream(&mut Slice(data), data.len() as u64, hash, &mut out)?;
    of_len(out, len)
}

/// The memory plan of `leveldb-recons` ([`aqr_api::stack`]): what it takes beyond its initial
/// memory at most, of inputs of `inputs` bytes (the second the layout `layout`) and an output of
/// `len`: what `aqr_api::whole` copies, the table and the compressor's table on top of it.
pub fn table_plan(inputs: [u64; 3], layout: &[u8], len: u64) -> Option<u64> {
    let own = leveldb::build_plan(inputs[0], layout, inputs[2], len)?;
    Some(aqr_api::stack::whole(1, &[], &inputs) + own)
}

/// The module `leveldb-recons`: a LevelDB table of `len` bytes (see `FORMAT.md`).
pub fn decode_table(params: &[u8], inputs: &[&[u8]], len: u64, _threads: usize) -> Result<Vec<u8>> {
    let (&[h], &[data, layout, kept]) = (params, inputs) else { return Err(corrupt()) };
    let hash = Hash::from_u8(h).ok_or_else(corrupt)?;
    of_len(leveldb::build_in(data, layout, kept, hash, cap(len))?, len)
}

/// The capacity of an output of `len` bytes (no larger but where it is not that output).
fn cap(len: u64) -> usize {
    usize::try_from(len).unwrap_or(usize::MAX)
}

fn of_len(out: Vec<u8>, len: u64) -> Result<Vec<u8>> {
    if out.len() as u64 != len {
        return Err(Error("output of another length"));
    }
    Ok(out)
}
