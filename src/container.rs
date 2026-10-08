//! The file format.
//!
//! ```text
//! "OBJR" version(u8) varint(length) u64-LE(xxh3 of the original)
//! blocks, until the length is reached:
//!   u8 mode, varint(raw length)
//!   mode 0 (whole): varint(coded length) zstd(block)
//!   mode 1 (coded): per stream (block::NSTREAMS):
//!                     varint(raw length) [u8(0 stored | 1 zstd | 2 rANS) varint(coded length) data]
//! ```
//!
//! Blocks end at a line end where there is one; coded blocks carry state over (previous
//! values, templates), so they are decoded in order.

use crate::block::{self, NSTREAMS};
use crate::ans;
use crate::varint::{Rd, put_uv};
use crate::{Error, Result, corrupt};
use std::io::Write;
use xxhash_rust::xxh3::{Xxh3, xxh3_64};

const MAGIC: &[u8; 4] = b"OBJR";
const VERSION: u8 = 1;
/// Largest block the decoder accepts (bounds its memory).
const MAX_BLOCK: usize = 64 << 20;

/// How a stream is stored.
const M_STORED: u8 = 0;
const M_ZSTD: u8 = 1;
/// rANS coded (`ans`).
const M_ANS: u8 = 2;

/// Streams of LEB128 values, which may be rANS coded.
const AC_STREAMS: [bool; NSTREAMS] = {
    let mut t = [false; NSTREAMS];
    let mut i = 0;
    while i < NSTREAMS {
        t[i] = i == block::S_SCALE || (i >= block::S_FESC && i < block::S_FESC + 3) || (i >= block::S_DUP && i < block::S_DUP + 3) || i >= block::S_VAL;
        i += 1;
    }
    t
};

#[derive(Clone, Copy, Debug)]
pub struct Options {
    pub level: i32,
    /// Input bytes per block.
    pub block: usize,
    /// Decode every block and compare (stored whole on a mismatch).
    pub verify: bool,
    /// For tests: blocks (index mod 64) stored whole as if verification failed.
    pub force_whole: u64,
}

impl Default for Options {
    fn default() -> Self {
        Options { level: 3, block: 4 << 20, verify: true, force_whole: 0 }
    }
}

/// Raw and coded sizes per stream, for analysis.
#[derive(Clone, Debug, Default)]
pub struct Stats {
    pub raw: [u64; NSTREAMS],
    pub coded: [u64; NSTREAMS],
    /// Blocks stored whole, and their coded size.
    pub whole: u64,
    /// Blocks rANS coded, per stream.
    pub rc: [u64; NSTREAMS],
    pub whole_coded: u64,
    pub transform_s: f64,
    pub zstd_s: f64,
    pub verify_s: f64,
}

pub fn compress(src: &[u8]) -> Vec<u8> {
    compress_with(src, &Options::default(), &mut Stats::default())
}

pub fn compress_with(src: &[u8], opt: &Options, st: &mut Stats) -> Vec<u8> {
    let mut out = Vec::new();
    compress_to(src, opt, st, &mut out).expect("writing to a Vec");
    out
}

/// Compresses `src` into `w`, a block at a time: memory besides the input stays bounded
/// by the block size.
pub fn compress_to<W: Write>(src: &[u8], opt: &Options, st: &mut Stats, w: &mut W) -> std::io::Result<()> {
    let mut out = Vec::new();
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
    let mut ac = Vec::new();
    // Per stream: the way that won clearly on the last block (M_STORED: none).
    let mut winner = [M_STORED; NSTREAMS];
    let mut cz = zstd::bulk::Compressor::new(opt.level).expect("zstd");
    let mut p = 0;
    let mut nblock = 0;
    while p < src.len() {
        w.write_all(&out)?;
        out.clear();
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
        let mut ok = opt.force_whole >> (nblock % 64) & 1 == 0;
        nblock += 1;
        if ok && opt.verify {
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
                // Both zstd and rANS for streams that may take either, unless one has won
                // clearly on the last block (tried again every few blocks).
                let recheck = nblock % 8 == 0;
                let try_ans = AC_STREAMS[i] && s.len() > 64 && (winner[i] != M_ZSTD || recheck);
                let try_zstd = s.len() > 64 && (winner[i] != M_ANS || recheck || !try_ans);
                z.clear();
                if try_zstd {
                    z.reserve(zstd::zstd_safe::compress_bound(s.len()));
                    cz.compress_to_buffer(s, &mut z).expect("zstd");
                }
                let (mut m, mut d) = if !z.is_empty() && z.len() < s.len() { (M_ZSTD, &z) } else { (M_STORED, s) };
                let mut margin = M_STORED;
                if try_ans {
                    // rANS coded, if smaller (and, verifying, if it decodes back).
                    ac.clear();
                    let ok = ans::encode(s, &mut ac).is_some()
                        && ac.len() < d.len()
                        && (!opt.verify || ans::decode(&ac, s.len(), &mut check) && check == *s);
                    if ok {
                        margin = if !try_zstd || ac.len() * 100 < d.len() * 97 { M_ANS } else { M_STORED };
                        (m, d) = (M_ANS, &ac);
                        st.rc[i] += 1;
                    } else if m == M_ZSTD && ac.len() * 100 > z.len() * 110 {
                        margin = M_ZSTD;
                    }
                }
                winner[i] = margin;
                out.push(m);
                put_uv(&mut out, d.len() as u64);
                out.extend_from_slice(d);
                st.coded[i] += d.len() as u64;
            }
        }
        st.zstd_s += t.elapsed().as_secs_f64();
    }
    w.write_all(&out)
}

pub fn decompress(src: &[u8]) -> Result<Vec<u8>> {
    let mut r = Rd::new(src);
    let len = if r.bytes(5).is_some() { r.uv() as usize } else { 0 };
    // The output is the caller's; reserve it only up to what the input can plausibly hold.
    let mut out = Vec::with_capacity(len.min(src.len().saturating_mul(64)));
    decompress_to(src, &mut out)?;
    Ok(out)
}

/// Decompresses `src` into `w`, a block at a time; returns the length. On an error, what
/// was written is not the original.
pub fn decompress_to<W: Write>(src: &[u8], w: &mut W) -> Result<usize> {
    let mut r = Rd::new(src);
    if r.bytes(4) != Some(MAGIC) || r.u8() != VERSION {
        return Err(Error("not an OBJR file"));
    }
    let len = r.uv() as usize;
    let hash = u64::from_le_bytes(r.bytes(8).ok_or_else(corrupt)?.try_into().unwrap());
    if r.overrun() {
        return Err(corrupt());
    }
    let mut out = Vec::new();
    let mut done = 0;
    let mut h = Xxh3::new();
    let mut dec = block::Decoder::default();
    let mut dz = zstd::bulk::Decompressor::new().map_err(|_| corrupt())?;
    let mut streams: [Vec<u8>; NSTREAMS] = Default::default();
    while done < len {
        out.clear();
        let mode = r.u8();
        let n = r.uv() as usize;
        if r.overrun() || n == 0 || n > MAX_BLOCK || n > len - done {
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
                for (i, s) in streams.iter_mut().enumerate() {
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
                        M_STORED if c == raw => s.extend_from_slice(d),
                        M_ANS if AC_STREAMS[i] => {
                            if !ans::decode(d, raw, s) {
                                return Err(corrupt());
                            }
                        }
                        M_ZSTD => {
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
        h.update(&out);
        w.write_all(&out).map_err(|_| Error("write error"))?;
        done += n;
    }
    if !r.done() || h.digest() != hash {
        return Err(Error("checksum mismatch"));
    }
    Ok(len)
}
