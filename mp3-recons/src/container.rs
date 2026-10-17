//! Container format and top-level compress / decompress.
//!
//! ```text
//! magic 69 04 7e 30 fd da ("aQR+MP3" in base64), version(u16 LE = 0), mode(u8)
//! mode 0 (stored): the input verbatim
//! mode 1 (coded):  u64 LE length, u64 LE xxh3 of the original, then per byte stream
//!                  varint(len << 1 | zstd) [varint raw len if zstd] data,
//!                  then varint bit lengths, the rANS words and the raw bits
//! ```

use crate::codec::{self, Coded, STREAM_NAMES};
use crate::par;
use recons_common::varint;
use xxhash_rust::xxh3::xxh3_64;

const MAGIC: [u8; 6] = [0x69, 0x04, 0x7e, 0x30, 0xfd, 0xda];
const VERSION: u16 = 0;
/// Magic, version and mode.
const HDR: usize = 9;
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
    let mut out = Vec::with_capacity(input.len() + HDR);
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.push(0);
    out.extend_from_slice(input);
    out
}

/// Per-stream sizes (raw, stored) in `STREAM_NAMES` order.
pub type Sizes = Vec<(usize, usize)>;

/// The stream compressed, if that makes it smaller.
pub(crate) fn pack(b: &[u8]) -> Option<Vec<u8>> {
    let z = if b.len() > 16 { zstd::bulk::compress(b, ZSTD_LEVEL).unwrap() } else { Vec::new() };
    (!z.is_empty() && z.len() < b.len()).then_some(z)
}

pub fn compress_sizes(input: &[u8]) -> (Vec<u8>, Sizes) {
    compress_sizes_threads(input, 1)
}

/// [`compress_sizes`] with `threads` threads (the same output).
pub fn compress_sizes_threads(input: &[u8], threads: usize) -> (Vec<u8>, Sizes) {
    let (c, packed, hash) = if threads > 1 && input.len() >= par::PAR_MIN {
        par::encode(input, pack)
    } else {
        let c = codec::encode(input);
        let packed = c.streams.iter().map(|b| pack(b)).collect();
        (c, packed, xxh3_64(input))
    };
    let mut out = Vec::with_capacity(input.len());
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.push(1);
    out.extend_from_slice(&(input.len() as u64).to_le_bytes());
    out.extend_from_slice(&hash.to_le_bytes());
    let mut sizes = Vec::with_capacity(STREAM_NAMES.len());
    for (b, z) in c.streams.iter().zip(&packed) {
        if let Some(z) = z {
            put_varint(&mut out, (z.len() as u64) << 1 | 1);
            put_varint(&mut out, b.len() as u64);
            out.extend_from_slice(z);
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
    if out.len() >= input.len() + HDR {
        return (stored(input), sizes);
    }
    (out, sizes)
}

pub fn compress(input: &[u8]) -> Vec<u8> {
    compress_threads(input, 1)
}

/// [`compress`] with `threads` threads: the same output whatever the number.
pub fn compress_threads(input: &[u8], threads: usize) -> Vec<u8> {
    match std::panic::catch_unwind(|| compress_sizes_threads(input, threads).0) {
        Ok(c) => c,
        Err(_) => stored(input),
    }
}

/// Magic and version (the mode byte is present).
fn is_ours(c: &[u8]) -> bool {
    c.len() >= HDR && c[..6] == MAGIC && u16::from_le_bytes([c[6], c[7]]) == VERSION
}

pub fn decompress(c: &[u8]) -> Result<Vec<u8>, Error> {
    if !is_ours(c) {
        return Err(Error("not an mp3-recons file".into()));
    }
    if c[HDR - 1] == 0 {
        return Ok(c[HDR..].to_vec());
    }
    if c[HDR - 1] != 1 {
        return Err(Error("unknown mode".into()));
    }
    let mut p = HDR;
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
            streams.push(par::unzstd(z, raw).map_err(Error)?);
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

/// [`decompress`] with `threads` threads: the same result whatever the number.
pub fn decompress_threads(c: &[u8], threads: usize) -> Result<Vec<u8>, Error> {
    if threads <= 1 || c.len() < par::PAR_MIN || !is_ours(c) || c[HDR - 1] != 1 {
        return decompress(c);
    }
    let mut srcs = Vec::with_capacity(STREAM_NAMES.len() - 2);
    // The container's layout; on an error, a stream before it that does not decompress
    // is reported first, as by `decompress`.
    let framing = (|| -> Result<_, Error> {
        let mut p = HDR;
        let len = u64::from_le_bytes(take(c, &mut p, 8)?.try_into().unwrap());
        let hash = u64::from_le_bytes(take(c, &mut p, 8)?.try_into().unwrap());
        for _ in 0..STREAM_NAMES.len() - 2 {
            let v = get_varint(c, &mut p)?;
            let n = (v >> 1) as usize;
            if v & 1 != 0 {
                let raw = get_varint(c, &mut p)? as usize;
                if raw as u64 > len + (1 << 20) {
                    return Err(Error("stream size".into()));
                }
                srcs.push(par::Src::Zstd(take(c, &mut p, n)?, raw));
            } else {
                srcs.push(par::Src::Plain(take(c, &mut p, n)?));
            }
        }
        for _ in 0..3 {
            get_varint(c, &mut p)?;
        }
        let nwords = get_varint(c, &mut p)? as usize;
        let _raw_bits = get_varint(c, &mut p)?;
        let words = take(c, &mut p, nwords.checked_mul(4).ok_or(Error("size".into()))?)?;
        Ok((len, hash, words, &c[p..]))
    })();
    let (len, hash, words, raw) = match framing {
        Ok(f) => f,
        Err(e) => {
            for s in &srcs {
                s.unpack().map_err(Error)?;
            }
            return Err(e);
        }
    };
    let srcs: &[par::Src; codec::N_BYTE_STREAMS] = srcs[..].try_into().unwrap();
    let out = par::decode(len, srcs, words, raw, threads).map_err(|e| match e {
        par::DecError::Stream(m) => Error(m),
        par::DecError::Codec(e) => Error(format!("corrupt: {}", e.0)),
    })?;
    if out.len() as u64 != len || xxh3_64(&out) != hash {
        return Err(Error("checksum mismatch".into()));
    }
    Ok(out)
}
