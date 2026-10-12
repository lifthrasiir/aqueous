//! Container format and top-level compress / decompress.
//!
//! ```text
//! "FLR" version(u8) mode(u8)
//! mode 0 (stored): the input verbatim
//! mode 1 (FLAC), 2 (PCM):
//!                  u64 LE length, u64 LE xxh3 of the original, then per byte stream
//!                  varint(len << 1 | zstd) [varint raw len if zstd] data,
//!                  then varint rANS word count, the words (LE), varint length of raw
//!                  stream 0, raw streams 0 and 1
//! ```

use crate::codec::{self, Coded, N_BYTE_STREAMS, Rd, put_varint};
use crate::pcm;
use xxhash_rust::xxh3::xxh3_64;

const MAGIC: &[u8; 3] = b"FLR";
const VERSION: u8 = 1;
/// zstd level of the byte streams.
const ZSTD_LEVEL: i32 = 3;

#[derive(Debug)]
pub struct Error(pub String);

fn stored(input: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(input.len() + 5);
    out.extend_from_slice(MAGIC);
    out.push(VERSION);
    out.push(MODE_STORED);
    out.extend_from_slice(input);
    out
}

/// Per-stream sizes (raw, stored) in `codec::STREAM_NAMES` order.
pub type Sizes = Vec<(usize, usize)>;

const MODE_STORED: u8 = 0;
const MODE_FLAC: u8 = 1;
const MODE_PCM: u8 = 2;

/// Serialises a coded file.
fn pack(input: &[u8], mode: u8, c: &Coded) -> (Vec<u8>, Sizes) {
    let mut out = Vec::with_capacity(input.len());
    out.extend_from_slice(MAGIC);
    out.push(VERSION);
    out.push(mode);
    out.extend_from_slice(&(input.len() as u64).to_le_bytes());
    out.extend_from_slice(&xxh3_64(input).to_le_bytes());
    let mut sizes = Vec::with_capacity(N_BYTE_STREAMS + 3);
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
    put_varint(&mut out, c.words.len() as u64);
    out.reserve(c.words.len() * 4 + c.raw.len());
    for w in &c.words {
        out.extend_from_slice(&w.to_le_bytes());
    }
    put_varint(&mut out, c.raw[0].len() as u64);
    out.extend_from_slice(&c.raw[0]);
    out.extend_from_slice(&c.raw[1]);
    sizes.push((c.words.len() * 4, c.words.len() * 4));
    sizes.push((c.raw[0].len(), c.raw[0].len()));
    sizes.push((c.raw[1].len(), c.raw[1].len()));
    (out, sizes)
}

/// Encodes FLAC, or WAV / AIFF (detected), or headerless PCM if `raw` is given.
fn encode(input: &[u8], raw: Option<pcm::Format>) -> Option<(u8, Coded)> {
    if let Some(fmt) = raw {
        return Some((MODE_PCM, pcm::encode(input, fmt, 0, input.len())?));
    }
    if let Some(c) = codec::encode(input) {
        return Some((MODE_FLAC, c));
    }
    let (fmt, start, len) = pcm::detect(input)?;
    Some((MODE_PCM, pcm::encode(input, fmt, start, len)?))
}

/// [`compress`], with the stream sizes.
pub fn compress_sizes(input: &[u8], raw: Option<pcm::Format>) -> (Vec<u8>, Sizes) {
    let Some((mode, c)) = encode(input, raw) else {
        return (stored(input), vec![(0, 0); N_BYTE_STREAMS + 3]);
    };
    let (out, sizes) = pack(input, mode, &c);
    if out.len() >= input.len() + 5 {
        return (stored(input), sizes);
    }
    (out, sizes)
}

/// Compresses any input: FLAC, WAV and AIFF are recognised; anything else is stored, as is
/// what makes the coder panic.
pub fn compress(input: &[u8]) -> Vec<u8> {
    compress_with(input, None)
}

/// [`compress`]; with `raw`, the whole input is taken as samples of that format.
pub fn compress_with(input: &[u8], raw: Option<pcm::Format>) -> Vec<u8> {
    match std::panic::catch_unwind(|| compress_sizes(input, raw).0) {
        Ok(c) => c,
        Err(_) => stored(input),
    }
}

/// True if the coded path (not the container's fallback) round-trips.
pub fn roundtrip(input: &[u8], raw: Option<pcm::Format>) -> bool {
    let Some((mode, c)) = encode(input, raw) else { return true };
    let mut out = Vec::new();
    let r = if mode == MODE_FLAC {
        codec::decode(&c, input.len(), &mut out)
    } else {
        pcm::decode(&c, input.len(), &mut out)
    };
    r.is_ok() && out == input
}

pub fn decompress(c: &[u8]) -> Result<Vec<u8>, Error> {
    if c.len() < 5 || &c[0..3] != MAGIC || c[3] != VERSION {
        return Err(Error("not an FLR file".into()));
    }
    let mode = c[4];
    match mode {
        MODE_STORED => return Ok(c[5..].to_vec()),
        MODE_FLAC | MODE_PCM => {}
        _ => return Err(Error("unknown mode".into())),
    }
    let e = |e: codec::Error| Error(format!("corrupt: {}", e.0));
    let mut r = Rd::new(&c[5..]);
    let len = u64::from_le_bytes(r.bytes(8).map_err(e)?.try_into().unwrap());
    let hash = u64::from_le_bytes(r.bytes(8).map_err(e)?.try_into().unwrap());
    // No output is larger than 16 times the input (minimum code length per residual).
    if len > (c.len() as u64).saturating_mul(64) + (1 << 20) {
        return Err(Error("length".into()));
    }
    let mut streams = Vec::with_capacity(N_BYTE_STREAMS);
    for _ in 0..N_BYTE_STREAMS {
        let v = r.varint().map_err(e)?;
        let n = (v >> 1) as usize;
        if v & 1 != 0 {
            let raw = r.varint().map_err(e)? as usize;
            if raw as u64 > len + (1 << 20) {
                return Err(Error("stream size".into()));
            }
            let z = r.bytes(n).map_err(e)?;
            streams.push(zstd::bulk::decompress(z, raw).map_err(|e| Error(e.to_string()))?);
        } else {
            streams.push(r.bytes(n).map_err(e)?.to_vec());
        }
    }
    let nwords = r.varint().map_err(e)? as usize;
    let words: Vec<u32> = r
        .bytes(nwords.checked_mul(4).ok_or(Error("size".into()))?)
        .map_err(e)?
        .chunks_exact(4)
        .map(|w| u32::from_le_bytes(w.try_into().unwrap()))
        .collect();
    let n0 = r.varint().map_err(e)? as usize;
    let raw0 = r.bytes(n0).map_err(e)?.to_vec();
    let raw = [raw0, r.d[r.p..].to_vec()];
    let coded = Coded { streams, words, raw };
    let mut out = Vec::new();
    if mode == MODE_FLAC {
        codec::decode(&coded, len as usize, &mut out).map_err(e)?;
    } else {
        pcm::decode(&coded, len as usize, &mut out).map_err(e)?;
    }
    if out.len() as u64 != len || xxh3_64(&out) != hash {
        return Err(Error("checksum mismatch".into()));
    }
    Ok(out)
}
