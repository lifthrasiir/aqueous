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

/// Options of [`compress_opt`].
#[derive(Clone, Copy, Debug)]
pub struct Options {
    /// Take the whole input as headerless PCM of this format.
    pub raw: Option<pcm::Format>,
    /// Threads (1: all on the calling thread). The output does not depend on it.
    pub threads: usize,
}

impl Default for Options {
    fn default() -> Self {
        Options { raw: None, threads: 1 }
    }
}

/// Streams at least this large are compressed / decompressed on threads of their own.
const PAR_STREAM: usize = 1 << 16;

fn zstd_compress(b: &[u8]) -> Vec<u8> {
    if b.len() > 16 { zstd::bulk::compress(b, ZSTD_LEVEL).unwrap() } else { Vec::new() }
}

/// Serialises a coded file (`hash`: of the input).
fn pack(input: &[u8], mode: u8, c: &Coded, hash: u64, threads: usize) -> (Vec<u8>, Sizes) {
    let mut out = Vec::with_capacity(input.len());
    out.extend_from_slice(MAGIC);
    out.push(VERSION);
    out.push(mode);
    out.extend_from_slice(&(input.len() as u64).to_le_bytes());
    out.extend_from_slice(&hash.to_le_bytes());
    let mut sizes = Vec::with_capacity(N_BYTE_STREAMS + 3);
    // With threads, the large streams on threads of their own.
    let mut zs: Vec<Option<Vec<u8>>> = vec![None; c.streams.len()];
    if threads > 1 {
        std::thread::scope(|sc| {
            let hs: Vec<_> = (c.streams.iter().zip(&mut zs))
                .filter(|(b, _)| b.len() >= PAR_STREAM)
                .map(|(b, z)| sc.spawn(move || *z = Some(zstd_compress(b))))
                .collect();
            for h in hs {
                h.join().unwrap_or_else(|p| std::panic::resume_unwind(p));
            }
        });
    }
    for (b, z) in c.streams.iter().zip(zs) {
        let z = z.unwrap_or_else(|| zstd_compress(b));
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

/// Encodes FLAC, or WAV / AIFF (detected), or headerless PCM if `opt.raw` is given.
fn encode(input: &[u8], opt: &Options) -> Option<(u8, Coded)> {
    let t = opt.threads;
    if let Some(fmt) = opt.raw {
        return Some((MODE_PCM, pcm::encode_threads(input, fmt, 0, input.len(), t)?));
    }
    if let Some(c) = codec::encode_threads(input, t) {
        return Some((MODE_FLAC, c));
    }
    let (fmt, start, len) = pcm::detect(input)?;
    Some((MODE_PCM, pcm::encode_threads(input, fmt, start, len, t)?))
}

fn decode(mode: u8, c: &Coded, len: usize, out: &mut Vec<u8>, threads: usize) -> Result<(), codec::Error> {
    if mode == MODE_FLAC {
        codec::decode_threads(c, len, out, threads)
    } else {
        pcm::decode_threads(c, len, out, threads)
    }
}

/// [`compress`], with the stream sizes.
pub fn compress_sizes(input: &[u8], raw: Option<pcm::Format>) -> (Vec<u8>, Sizes) {
    compress_sizes_opt(input, &Options { raw, threads: 1 })
}

/// [`compress_opt`], with the stream sizes (and without the fallback on panics).
pub fn compress_sizes_opt(input: &[u8], opt: &Options) -> (Vec<u8>, Sizes) {
    let (coded, hash) = if opt.threads > 1 {
        // The checksum on a thread of its own, if it is needed.
        std::thread::scope(|sc| {
            let h = sc.spawn(|| xxh3_64(input));
            let coded = encode(input, opt);
            (coded, h.join().unwrap_or_else(|p| std::panic::resume_unwind(p)))
        })
    } else {
        let Some(coded) = encode(input, opt) else {
            return (stored(input), vec![(0, 0); N_BYTE_STREAMS + 3]);
        };
        (Some(coded), xxh3_64(input))
    };
    let Some((mode, c)) = coded else {
        return (stored(input), vec![(0, 0); N_BYTE_STREAMS + 3]);
    };
    let (out, sizes) = pack(input, mode, &c, hash, opt.threads);
    if out.len() >= input.len() + 5 {
        return (stored(input), sizes);
    }
    (out, sizes)
}

/// Compresses any input: FLAC, WAV and AIFF are recognised; anything else is stored, as is
/// what makes the coder panic.
pub fn compress(input: &[u8]) -> Vec<u8> {
    compress_opt(input, &Options::default())
}

/// [`compress`]; with `raw`, the whole input is taken as samples of that format.
pub fn compress_with(input: &[u8], raw: Option<pcm::Format>) -> Vec<u8> {
    compress_opt(input, &Options { raw, threads: 1 })
}

/// [`compress`] with options; the output is the same whatever the number of threads.
pub fn compress_opt(input: &[u8], opt: &Options) -> Vec<u8> {
    match std::panic::catch_unwind(|| compress_sizes_opt(input, opt).0) {
        Ok(c) => c,
        Err(_) => stored(input),
    }
}

/// True if the coded path (not the container's fallback) round-trips.
pub fn roundtrip(input: &[u8], raw: Option<pcm::Format>) -> bool {
    roundtrip_opt(input, &Options { raw, threads: 1 })
}

/// [`roundtrip`] with options (the threads for both ways).
pub fn roundtrip_opt(input: &[u8], opt: &Options) -> bool {
    let Some((mode, c)) = encode(input, opt) else { return true };
    let mut out = Vec::new();
    decode(mode, &c, input.len(), &mut out, opt.threads).is_ok() && out == input
}

pub fn decompress(c: &[u8]) -> Result<Vec<u8>, Error> {
    decompress_threads(c, 1)
}

/// [`decompress`] with `threads` threads; the same result whatever their number.
pub fn decompress_threads(c: &[u8], threads: usize) -> Result<Vec<u8>, Error> {
    if c.len() < 5 || &c[0..3] != MAGIC || c[3] != VERSION {
        return Err(Error("not an FLR file".into()));
    }
    let mode = c[4];
    match mode {
        MODE_STORED => return Ok(c[5..].to_vec()),
        MODE_FLAC | MODE_PCM => {}
        _ => return Err(Error("unknown mode".into())),
    }
    let e = corrupt;
    let mut r = Rd::new(&c[5..]);
    let len = u64::from_le_bytes(r.bytes(8).map_err(e)?.try_into().unwrap());
    let hash = u64::from_le_bytes(r.bytes(8).map_err(e)?.try_into().unwrap());
    // No output is larger than 16 times the input (minimum code length per residual).
    if len > (c.len() as u64).saturating_mul(64) + (1 << 20) {
        return Err(Error("length".into()));
    }
    let streams = if threads > 1 {
        byte_streams_par(&mut r, len)?
    } else {
        let mut streams = Vec::with_capacity(N_BYTE_STREAMS);
        for _ in 0..N_BYTE_STREAMS {
            streams.push(match byte_stream(&mut r, len)? {
                (z, Some(raw)) => zstd_decompress(z, raw)?,
                (b, None) => b.to_vec(),
            });
        }
        streams
    };
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
    decode(mode, &coded, len as usize, &mut out, threads).map_err(e)?;
    if out.len() as u64 != len || xxh3_64(&out) != hash {
        return Err(Error("checksum mismatch".into()));
    }
    Ok(out)
}

fn corrupt(e: codec::Error) -> Error {
    Error(format!("corrupt: {}", e.0))
}

/// The next byte stream of a container: its bytes, and its size if they are zstd'd.
fn byte_stream<'a>(r: &mut Rd<'a>, len: u64) -> Result<(&'a [u8], Option<usize>), Error> {
    let v = r.varint().map_err(corrupt)?;
    let n = (v >> 1) as usize;
    if v & 1 != 0 {
        let raw = r.varint().map_err(corrupt)? as usize;
        if raw as u64 > len + (1 << 20) {
            return Err(Error("stream size".into()));
        }
        Ok((r.bytes(n).map_err(corrupt)?, Some(raw)))
    } else {
        Ok((r.bytes(n).map_err(corrupt)?, None))
    }
}

fn zstd_decompress(z: &[u8], raw: usize) -> Result<Vec<u8>, Error> {
    zstd::bulk::decompress(z, raw).map_err(|e| Error(e.to_string()))
}

/// The byte streams, the large ones decompressed on threads of their own; the same
/// result (and first error) as one after the other.
fn byte_streams_par(r: &mut Rd, len: u64) -> Result<Vec<Vec<u8>>, Error> {
    let mut parts = Vec::with_capacity(N_BYTE_STREAMS);
    let mut err = None;
    for _ in 0..N_BYTE_STREAMS {
        match byte_stream(r, len) {
            Ok(p) => parts.push(p),
            Err(e) => {
                err = Some(e);
                break;
            }
        }
    }
    let mut res: Vec<Option<Result<Vec<u8>, Error>>> = (0..parts.len()).map(|_| None).collect();
    std::thread::scope(|sc| {
        let hs: Vec<_> = (parts.iter().zip(&mut res))
            .filter(|((z, raw), _)| raw.is_some() && z.len() >= PAR_STREAM / 4)
            .map(|(&(z, raw), slot)| sc.spawn(move || *slot = Some(zstd_decompress(z, raw.unwrap()))))
            .collect();
        for h in hs {
            h.join().unwrap_or_else(|p| std::panic::resume_unwind(p));
        }
    });
    let mut streams = Vec::with_capacity(N_BYTE_STREAMS);
    for (&(b, raw), done) in parts.iter().zip(res) {
        streams.push(match (done, raw) {
            (Some(d), _) => d?,
            (None, Some(raw)) => zstd_decompress(b, raw)?,
            (None, None) => b.to_vec(),
        });
    }
    match err {
        Some(e) => Err(e),
        None => Ok(streams),
    }
}
