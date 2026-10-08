//! The file format.
//!
//! ```text
//! "OBJR" version(u8) varint(length) u64-LE(xxh3 of the original)
//! blocks, until the length is reached:
//!   u8 mode, varint(raw length)
//!   mode 0 (whole): varint(coded length) zstd(block)
//!   mode 1 (coded): per stream (block::NSTREAMS):
//!                     varint(raw length) [u8(0 stored | 1 zstd) varint(coded length) data]
//! ```
//!
//! Blocks end at a line end where there is one; coded blocks carry state over (previous
//! values, templates), so they are decoded in order.

use crate::block::{self, NSTREAMS};
use crate::varint::{Rd, put_uv};
use crate::{Error, Result, corrupt};
use xxhash_rust::xxh3::xxh3_64;

const MAGIC: &[u8; 4] = b"OBJR";
const VERSION: u8 = 1;
/// Largest block the decoder accepts (bounds its memory).
const MAX_BLOCK: usize = 64 << 20;

#[derive(Clone, Copy, Debug)]
pub struct Options {
    pub level: i32,
    /// Input bytes per block.
    pub block: usize,
    /// Decode every block and compare (stored whole on a mismatch).
    pub verify: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options { level: 3, block: 4 << 20, verify: true }
    }
}

/// Raw and coded sizes per stream, for analysis.
#[derive(Clone, Debug, Default)]
pub struct Stats {
    pub raw: [u64; NSTREAMS],
    pub coded: [u64; NSTREAMS],
    /// Blocks stored whole, and their coded size.
    pub whole: u64,
    pub whole_coded: u64,
    pub transform_s: f64,
    pub zstd_s: f64,
    pub verify_s: f64,
}

pub fn compress(src: &[u8]) -> Vec<u8> {
    compress_with(src, &Options::default(), &mut Stats::default())
}

pub fn compress_with(src: &[u8], opt: &Options, st: &mut Stats) -> Vec<u8> {
    let mut out = Vec::with_capacity(src.len() / 4 + 64);
    out.extend_from_slice(MAGIC);
    out.push(VERSION);
    put_uv(&mut out, src.len() as u64);
    out.extend_from_slice(&xxh3_64(src).to_le_bytes());
    let bsize = opt.block.clamp(1 << 10, MAX_BLOCK);
    let mut enc = block::Encoder::default();
    let mut dec = block::Decoder::default();
    let mut streams: [Vec<u8>; NSTREAMS] = Default::default();
    let mut check = Vec::new();
    let mut z = Vec::new();
    let mut cz = zstd::bulk::Compressor::new(opt.level).expect("zstd");
    let mut p = 0;
    while p < src.len() {
        let mut e = (p + bsize).min(src.len());
        if e < src.len() {
            if let Some(i) = memchr::memrchr(b'\n', &src[p..e]) {
                e = p + i + 1;
            }
        }
        let b = &src[p..e];
        p = e;
        let t = std::time::Instant::now();
        let (em, dm) = (enc.mark(), dec.mark());
        enc.encode(b, &mut streams);
        st.transform_s += t.elapsed().as_secs_f64();
        let mut ok = true;
        if opt.verify {
            let t = std::time::Instant::now();
            check.clear();
            ok = dec.decode(&streams, &mut check) && check == b;
            st.verify_s += t.elapsed().as_secs_f64();
        }
        let t = std::time::Instant::now();
        if !ok {
            enc.reset(&em);
            dec.reset(&dm);
            out.push(0);
            put_uv(&mut out, b.len() as u64);
            z.clear();
            z.reserve(zstd::zstd_safe::compress_bound(b.len()));
            cz.compress_to_buffer(b, &mut z).expect("zstd");
            put_uv(&mut out, z.len() as u64);
            out.extend_from_slice(&z);
            st.whole += b.len() as u64;
            st.whole_coded += z.len() as u64;
        } else {
            out.push(1);
            put_uv(&mut out, b.len() as u64);
            for (i, s) in streams.iter().enumerate() {
                put_uv(&mut out, s.len() as u64);
                st.raw[i] += s.len() as u64;
                if s.is_empty() {
                    continue;
                }
                z.clear();
                if s.len() > 64 {
                    z.reserve(zstd::zstd_safe::compress_bound(s.len()));
                    cz.compress_to_buffer(s, &mut z).expect("zstd");
                }
                let (m, d) = if !z.is_empty() && z.len() < s.len() { (1, &z) } else { (0, s) };
                out.push(m);
                put_uv(&mut out, d.len() as u64);
                out.extend_from_slice(d);
                st.coded[i] += d.len() as u64;
            }
        }
        st.zstd_s += t.elapsed().as_secs_f64();
    }
    out
}

pub fn decompress(src: &[u8]) -> Result<Vec<u8>> {
    let mut r = Rd::new(src);
    if r.bytes(4) != Some(MAGIC) || r.u8() != VERSION {
        return Err(Error("not an OBJR file"));
    }
    let len = r.uv() as usize;
    let hash = u64::from_le_bytes(r.bytes(8).ok_or_else(corrupt)?.try_into().unwrap());
    if r.overrun() {
        return Err(corrupt());
    }
    // The output is the caller's; reserve it only up to what the input can plausibly hold.
    let mut out = Vec::with_capacity(len.min(src.len().saturating_mul(64)));
    let mut dec = block::Decoder::default();
    let mut dz = zstd::bulk::Decompressor::new().map_err(|_| corrupt())?;
    let mut streams: [Vec<u8>; NSTREAMS] = Default::default();
    while out.len() < len {
        let mode = r.u8();
        let n = r.uv() as usize;
        if r.overrun() || n == 0 || n > MAX_BLOCK || n > len - out.len() {
            return Err(corrupt());
        }
        match mode {
            0 => {
                let c = r.uv() as usize;
                let d = r.bytes(c).ok_or_else(corrupt)?;
                let s = out.len();
                out.reserve(n);
                dz.decompress_to_buffer(d, &mut out).map_err(|_| corrupt())?;
                if out.len() - s != n {
                    return Err(corrupt());
                }
            }
            1 => {
                for s in streams.iter_mut() {
                    s.clear();
                    let raw = r.uv() as usize;
                    if raw == 0 {
                        continue;
                    }
                    let m = r.u8();
                    let c = r.uv() as usize;
                    if raw > 8 * n + 64 {
                        return Err(corrupt());
                    }
                    let d = r.bytes(c).ok_or_else(corrupt)?;
                    match m {
                        0 if c == raw => s.extend_from_slice(d),
                        1 => {
                            s.reserve(raw);
                            dz.decompress_to_buffer(d, s).map_err(|_| corrupt())?;
                            if s.len() != raw {
                                return Err(corrupt());
                            }
                        }
                        _ => return Err(corrupt()),
                    }
                }
                let s = out.len();
                if !dec.decode(&streams, &mut out) || out.len() - s != n {
                    return Err(corrupt());
                }
            }
            _ => return Err(corrupt()),
        }
    }
    if !r.done() || xxh3_64(&out) != hash {
        return Err(Error("checksum mismatch"));
    }
    Ok(out)
}
