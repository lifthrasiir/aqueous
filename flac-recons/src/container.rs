//! Container format and top-level compress / decompress.
//!
//! ```text
//! magic(6: FLAC, WAV / AIFF or raw PCM) version(u16 LE)
//! version 0 (stored): the input verbatim
//! version 1 (coded):  u64 LE length, u64 LE xxh3 of the original, then per byte stream
//!                     varint(len << 1 | zstd) [varint raw len if zstd] data,
//!                     then varint rANS word count, the words (LE), varint length of raw
//!                     stream 0, raw streams 0 and 1 (FLAC streams under the FLAC magic,
//!                     PCM streams under the others)
//! ```
//!
//! FORMAT.md describes the format in full.

use crate::codec::{self, Coded, N_BYTE_STREAMS, Rd, put_varint};
use crate::{flac, pcm};
use xxhash_rust::xxh3::xxh3_64;

/// Magic of each kind of input (base64 `aQR+f1ac`, `aQR+wav3`, `aQR+pcm/`).
const MAGIC_FLAC: [u8; 6] = [0x69, 0x04, 0x7e, 0x7f, 0x56, 0x9c];
const MAGIC_WAV: [u8; 6] = [0x69, 0x04, 0x7e, 0xc1, 0xab, 0xf7];
const MAGIC_PCM: [u8; 6] = [0x69, 0x04, 0x7e, 0xa5, 0xc9, 0xbf];
const V_STORED: u16 = 0;
const V_CODED: u16 = 1;
/// Magic and version.
pub const HEADER_LEN: usize = 8;
/// zstd level of the byte streams.
const ZSTD_LEVEL: i32 = 3;

#[derive(Debug)]
pub struct Error(pub String);

/// What the input is: FLAC, WAV / AIFF (both coded as PCM), or raw PCM (also anything
/// unrecognised, which is stored).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Kind {
    Flac,
    Wav,
    Pcm,
}

impl Kind {
    fn magic(self) -> [u8; 6] {
        match self {
            Kind::Flac => MAGIC_FLAC,
            Kind::Wav => MAGIC_WAV,
            Kind::Pcm => MAGIC_PCM,
        }
    }

    fn from_magic(m: &[u8]) -> Option<Kind> {
        [Kind::Flac, Kind::Wav, Kind::Pcm].into_iter().find(|k| k.magic() == m)
    }

    /// The kind [`encode`] codes the input as.
    pub(crate) fn of(input: &[u8], opt: &Options) -> Kind {
        if opt.raw.is_some() {
            Kind::Pcm
        } else if flac::parse_metadata(input).is_some() {
            Kind::Flac
        } else if pcm::detect(input).is_some() {
            Kind::Wav
        } else {
            Kind::Pcm
        }
    }
}

fn header(out: &mut Vec<u8>, kind: Kind, version: u16) {
    out.extend_from_slice(&kind.magic());
    out.extend_from_slice(&version.to_le_bytes());
}

fn stored(input: &[u8], kind: Kind) -> Vec<u8> {
    let mut out = Vec::with_capacity(input.len() + HEADER_LEN);
    header(&mut out, kind, V_STORED);
    out.extend_from_slice(input);
    out
}

/// Per-stream sizes (raw, stored) in `codec::STREAM_NAMES` order.
pub type Sizes = Vec<(usize, usize)>;

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
fn pack(input: &[u8], kind: Kind, c: &Coded, hash: u64, threads: usize) -> (Vec<u8>, Sizes) {
    let mut out = Vec::with_capacity(input.len());
    header(&mut out, kind, V_CODED);
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
/// (`kind` is [`Kind::of`] the input.)
pub(crate) fn encode(input: &[u8], opt: &Options, kind: Kind) -> Option<Coded> {
    let t = opt.threads;
    match (kind, opt.raw) {
        (_, Some(fmt)) => pcm::encode_threads(input, fmt, 0, input.len(), t),
        (Kind::Flac, None) => codec::encode_threads(input, t),
        (Kind::Wav, None) => {
            let (fmt, start, len) = pcm::detect(input)?;
            pcm::encode_threads(input, fmt, start, len, t)
        }
        (Kind::Pcm, None) => None,
    }
}

pub(crate) fn decode(kind: Kind, c: &Coded, len: usize, out: &mut Vec<u8>, threads: usize) -> Result<(), codec::Error> {
    if kind == Kind::Flac {
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
    compress_kind(input, opt, Kind::of(input, opt))
}

fn compress_kind(input: &[u8], opt: &Options, kind: Kind) -> (Vec<u8>, Sizes) {
    let (coded, hash) = if opt.threads > 1 {
        // The checksum on a thread of its own, if it is needed.
        std::thread::scope(|sc| {
            let h = sc.spawn(|| xxh3_64(input));
            let coded = encode(input, opt, kind);
            (coded, h.join().unwrap_or_else(|p| std::panic::resume_unwind(p)))
        })
    } else {
        let Some(coded) = encode(input, opt, kind) else {
            return (stored(input, kind), vec![(0, 0); N_BYTE_STREAMS + 3]);
        };
        (Some(coded), xxh3_64(input))
    };
    let Some(c) = coded else {
        return (stored(input, kind), vec![(0, 0); N_BYTE_STREAMS + 3]);
    };
    let (out, sizes) = pack(input, kind, &c, hash, opt.threads);
    if out.len() >= input.len() + HEADER_LEN {
        return (stored(input, kind), sizes);
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
    // The kind decides the magic of the stored fallback too.
    let kind = std::panic::catch_unwind(|| Kind::of(input, opt)).unwrap_or(Kind::Pcm);
    match std::panic::catch_unwind(|| compress_kind(input, opt, kind).0) {
        Ok(c) => c,
        Err(_) => stored(input, kind),
    }
}

/// True if the coded path (not the container's fallback) round-trips.
pub fn roundtrip(input: &[u8], raw: Option<pcm::Format>) -> bool {
    roundtrip_opt(input, &Options { raw, threads: 1 })
}

/// [`roundtrip`] with options (the threads for both ways).
pub fn roundtrip_opt(input: &[u8], opt: &Options) -> bool {
    let kind = Kind::of(input, opt);
    let Some(c) = encode(input, opt, kind) else { return true };
    let mut out = Vec::new();
    decode(kind, &c, input.len(), &mut out, opt.threads).is_ok() && out == input
}

pub fn decompress(c: &[u8]) -> Result<Vec<u8>, Error> {
    decompress_threads(c, 1)
}

/// [`decompress`] with `threads` threads; the same result whatever their number.
pub fn decompress_threads(c: &[u8], threads: usize) -> Result<Vec<u8>, Error> {
    let kind = c.get(..6).and_then(Kind::from_magic).ok_or(Error("not an flr file".into()))?;
    match c.get(6..8).map(|v| u16::from_le_bytes([v[0], v[1]])) {
        Some(V_STORED) => return Ok(c[HEADER_LEN..].to_vec()),
        Some(V_CODED) => {}
        Some(v) => return Err(Error(format!("unknown version {v}"))),
        None => return Err(Error("truncated header".into())),
    }
    let e = corrupt;
    let mut r = Rd::new(&c[HEADER_LEN..]);
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
    decode(kind, &coded, len as usize, &mut out, threads).map_err(e)?;
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
