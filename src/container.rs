//! The file format.
//!
//! ```text
//! "SQLR" version(u8) mode(u8) varint(length) u64-LE(xxh3 of the original)
//! mode 0 (whole): zstd(file)                 -- not a database we can code
//! mode 1 (coded): varint(page size) varint(usable size) u8(schema format >= 4)
//!                 per stream (transform::NSTREAMS):
//!                   varint(raw length) u8(0 stored | 1 zstd) varint(coded length) data
//! ```

use crate::page::Geom;
use crate::transform::{self, NSTREAMS};
use crate::varint::{Rd, put_uv};
use crate::{Result, corrupt};
use xxhash_rust::xxh3::xxh3_64;

const MAGIC: &[u8; 4] = b"SQLR";
const VERSION: u8 = 1;

#[derive(Clone, Copy, Debug)]
pub struct Options {
    pub level: i32,
    /// Level of the text/blob stream (most of the output).
    pub level_bytes: i32,
    /// Decode the result and compare (falls back to mode 0 on a mismatch).
    pub verify: bool,
    pub ldm: bool,
    /// Store the file whole if the transform panics (off to find such bugs).
    pub catch: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options { level: 7, level_bytes: 9, verify: true, ldm: true, catch: true }
    }
}

/// Raw and coded sizes per stream, for analysis.
#[derive(Clone, Debug, Default)]
pub struct Stats {
    pub raw: [u64; NSTREAMS],
    pub coded: [u64; NSTREAMS],
    pub whole: bool,
    pub transform_s: f64,
    pub zstd_s: f64,
    pub verify_s: f64,
}

pub fn compress(src: &[u8]) -> Vec<u8> {
    compress_with(src, &Options::default(), &mut Stats::default())
}

fn header(out: &mut Vec<u8>, mode: u8, src: &[u8]) {
    out.extend_from_slice(MAGIC);
    out.push(VERSION);
    out.push(mode);
    put_uv(out, src.len() as u64);
    out.extend_from_slice(&xxh3_64(src).to_le_bytes());
}

fn whole(src: &[u8], level: i32) -> Vec<u8> {
    let mut out = Vec::new();
    header(&mut out, 0, src);
    out.extend_from_slice(&zstd::bulk::compress(src, level).expect("zstd"));
    out
}

pub fn compress_with(src: &[u8], opt: &Options, st: &mut Stats) -> Vec<u8> {
    let t = std::time::Instant::now();
    let enc = if opt.catch {
        std::panic::catch_unwind(|| transform::encode(src)).ok().flatten()
    } else {
        transform::encode(src)
    };
    st.transform_s += t.elapsed().as_secs_f64();
    let Some(enc) = enc else {
        st.whole = true;
        return whole(src, opt.level);
    };
    let t = std::time::Instant::now();
    let mut out = Vec::new();
    header(&mut out, 1, src);
    let g = enc.geom;
    put_uv(&mut out, g.psize as u64);
    put_uv(&mut out, g.usable as u64);
    out.push(g.fmt4 as u8);
    let mk = |level| {
        let mut cz = zstd::bulk::Compressor::new(level).expect("zstd");
        // Long-distance matching for what lies beyond the window.
        if opt.ldm && src.len() > 2 << 20 {
            use zstd::zstd_safe::CParameter as P;
            cz.set_parameter(P::EnableLongDistanceMatching(true)).expect("zstd");
            cz.set_parameter(P::WindowLog(27)).expect("zstd");
        }
        cz
    };
    let mut cz = mk(opt.level);
    let mut czb = mk(opt.level_bytes);
    for (i, s) in enc.streams.iter().enumerate() {
        put_uv(&mut out, s.len() as u64);
        st.raw[i] += s.len() as u64;
        let c = if i == transform::S_BYTES { &mut czb } else { &mut cz };
        let z = if s.len() > 64 { c.compress(s).expect("zstd") } else { Vec::new() };
        if !z.is_empty() && z.len() < s.len() {
            out.push(1);
            put_uv(&mut out, z.len() as u64);
            out.extend_from_slice(&z);
            st.coded[i] += z.len() as u64;
        } else {
            out.push(0);
            put_uv(&mut out, s.len() as u64);
            out.extend_from_slice(s);
            st.coded[i] += s.len() as u64;
        }
    }
    st.zstd_s += t.elapsed().as_secs_f64();
    if opt.verify {
        // zstd is trusted: rebuild the file from the streams.
        let t = std::time::Instant::now();
        let ok = matches!(transform::decode(&g, src.len(), &enc.streams), Ok(d) if d == src);
        st.verify_s += t.elapsed().as_secs_f64();
        if !ok {
            st.whole = true;
            return whole(src, opt.level);
        }
    }
    out
}

pub fn decompress(src: &[u8]) -> Result<Vec<u8>> {
    let mut r = Rd::new(src);
    if r.bytes(4)? != MAGIC || r.u8()? != VERSION {
        return Err(crate::Error("not an SQLR file"));
    }
    let mode = r.u8()?;
    let len = r.usize()?;
    let hash = u64::from_le_bytes(r.bytes(8)?.try_into().unwrap());
    let out = match mode {
        0 => {
            let mut out = Vec::with_capacity(len);
            zstd::stream::copy_decode(&src[r.p..], &mut out).map_err(|_| corrupt())?;
            out
        }
        1 => {
            let psize = r.usize()?;
            let usable = r.usize()?;
            let fmt4 = r.u8()? != 0;
            if !(512..=65536).contains(&psize) || !(480..=psize).contains(&usable) {
                return Err(corrupt());
            }
            let g = Geom { psize, usable, npages: len / psize, fmt4 };
            let mut dz = zstd::bulk::Decompressor::new().map_err(|_| corrupt())?;
            dz.set_parameter(zstd::zstd_safe::DParameter::WindowLogMax(27)).map_err(|_| corrupt())?;
            let mut streams = Vec::with_capacity(NSTREAMS);
            for _ in 0..NSTREAMS {
                let n = r.usize()?;
                let m = r.u8()?;
                let c = r.usize()?;
                let d = r.bytes(c)?;
                streams.push(match m {
                    0 if c == n => d.to_vec(),
                    1 => {
                        if n > len.saturating_mul(2) + (1 << 20) {
                            return Err(corrupt());
                        }
                        let mut v = Vec::with_capacity(n);
                        dz.decompress_to_buffer(d, &mut v).map_err(|_| corrupt())?;
                        if v.len() != n {
                            return Err(corrupt());
                        }
                        v
                    }
                    _ => return Err(corrupt()),
                });
            }
            transform::decode(&g, len, &streams)?
        }
        _ => return Err(corrupt()),
    };
    if out.len() != len || xxh3_64(&out) != hash {
        return Err(crate::Error("checksum mismatch"));
    }
    Ok(out)
}
