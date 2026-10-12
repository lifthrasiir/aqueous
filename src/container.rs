//! Container format and top-level compress / decompress.
//!
//! ```text
//! "M3R" version(u8) mode(u8)
//! mode 0 (stored): the input verbatim
//! mode 1 (coded):  u64 LE length, u64 LE xxh3 of the original, then per byte stream
//!                  varint(len << 1 | zstd) [varint raw len if zstd] data,
//!                  then varint bit lengths, the rANS words and the raw bits
//! ```

use crate::codec::{self, Coded, STREAM_NAMES};
use recons_common::varint;
use xxhash_rust::xxh3::xxh3_64;

const MAGIC: &[u8; 3] = b"M3R";
const VERSION: u8 = 1;
/// zstd level of the byte streams.
const ZSTD_LEVEL: i32 = 1;

#[derive(Debug)]
pub struct Error(pub String);

fn put_varint(out: &mut Vec<u8>, v: u64) {
    varint::put(out, v)
}

fn get_varint(d: &[u8], p: &mut usize) -> Result<u64, Error> {
    varint::get(d, p).map_err(|e| Error(e.0.into()))
}

fn take<'a>(d: &'a [u8], p: &mut usize, n: usize) -> Result<&'a [u8], Error> {
    let s = d.get(*p..*p + n).ok_or(Error("truncated".into()))?;
    *p += n;
    Ok(s)
}

fn stored(input: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(input.len() + 5);
    out.extend_from_slice(MAGIC);
    out.push(VERSION);
    out.push(0);
    out.extend_from_slice(input);
    out
}

/// Per-stream sizes (raw, stored) in `STREAM_NAMES` order.
pub type Sizes = Vec<(usize, usize)>;

pub fn compress_sizes(input: &[u8]) -> (Vec<u8>, Sizes) {
    let c = codec::encode(input);
    let mut out = Vec::with_capacity(input.len());
    out.extend_from_slice(MAGIC);
    out.push(VERSION);
    out.push(1);
    out.extend_from_slice(&(input.len() as u64).to_le_bytes());
    out.extend_from_slice(&xxh3_64(input).to_le_bytes());
    let mut sizes = Vec::with_capacity(STREAM_NAMES.len());
    for b in &c.streams {
        let z = if b.len() > 16 { zstd::bulk::compress(b, ZSTD_LEVEL).unwrap() } else { Vec::new() };
        if !z.is_empty() && z.len() < b.len() {
            put_varint(&mut out, (z.len() as u64) << 1 | 1);
            put_varint(&mut out, b.len() as u64);
            out.extend_from_slice(&z);
            sizes.push((b.len(), z.len()));
        } else {
            put_varint(&mut out, (b.len() as u64) << 1);
            out.extend_from_slice(b);
            sizes.push((b.len(), b.len()));
        }
    }
    for l in c.bit_lens {
        put_varint(&mut out, l);
    }
    put_varint(&mut out, c.words.len() as u64);
    put_varint(&mut out, c.raw_bits);
    for w in &c.words {
        out.extend_from_slice(&w.to_le_bytes());
    }
    out.extend_from_slice(&c.raw);
    sizes.push((c.words.len() * 4, c.words.len() * 4));
    sizes.push((c.raw.len(), c.raw.len()));
    if out.len() >= input.len() + 5 {
        return (stored(input), sizes);
    }
    (out, sizes)
}

pub fn compress(input: &[u8]) -> Vec<u8> {
    match std::panic::catch_unwind(|| compress_sizes(input).0) {
        Ok(c) => c,
        Err(_) => stored(input),
    }
}

pub fn decompress(c: &[u8]) -> Result<Vec<u8>, Error> {
    if c.len() < 5 || &c[0..3] != MAGIC || c[3] != VERSION {
        return Err(Error("not an M3R file".into()));
    }
    if c[4] == 0 {
        return Ok(c[5..].to_vec());
    }
    let mut p = 5;
    let len = u64::from_le_bytes(take(c, &mut p, 8)?.try_into().unwrap());
    let hash = u64::from_le_bytes(take(c, &mut p, 8)?.try_into().unwrap());
    let mut streams = Vec::with_capacity(STREAM_NAMES.len());
    for _ in 0..STREAM_NAMES.len() - 2 {
        let v = get_varint(c, &mut p)?;
        let n = (v >> 1) as usize;
        if v & 1 != 0 {
            let raw = get_varint(c, &mut p)? as usize;
            // No stream is larger than the original plus a little.
            if raw as u64 > len + (1 << 20) {
                return Err(Error("stream size".into()));
            }
            let z = take(c, &mut p, n)?;
            streams.push(zstd::bulk::decompress(z, raw).map_err(|e| Error(e.to_string()))?);
        } else {
            streams.push(take(c, &mut p, n)?.to_vec());
        }
    }
    let mut bit_lens = [0u64; 3];
    for b in &mut bit_lens {
        *b = get_varint(c, &mut p)?;
    }
    let nwords = get_varint(c, &mut p)? as usize;
    let raw_bits = get_varint(c, &mut p)?;
    let words: Vec<u32> = take(c, &mut p, nwords.checked_mul(4).ok_or(Error("size".into()))?)?
        .chunks_exact(4)
        .map(|w| u32::from_le_bytes(w.try_into().unwrap()))
        .collect();
    let raw = c[p..].to_vec();
    let coded = Coded { len, streams, words, raw, raw_bits, bit_lens };
    let out = codec::decode(&coded).map_err(|e| Error(format!("corrupt: {}", e.0)))?;
    if out.len() as u64 != len || xxh3_64(&out) != hash {
        return Err(Error("checksum mismatch".into()));
    }
    Ok(out)
}
