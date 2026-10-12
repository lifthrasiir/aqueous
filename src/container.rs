//! The file format.
//!
//! ```text
//! "SQLR" version(u8) mode(u8) varint(length) u64-LE(xxh3 of the original)
//! mode 0 (whole): zstd(file)                 -- not a database we can code
//! mode 1 (coded): varint(page size) varint(usable size) u8(schema format >= 4)
//!                 per stream (transform::NSTREAMS):
//!                   varint(raw length) u8(0 stored | 1 zstd) varint(coded length) data
//! ```
//!
//! With more than one thread, compression verifies while it compresses the streams (each
//! stream on a worker, with a compressor of its own; zstd itself stays single-threaded, so
//! the bytes do not depend on the threads), and decompression decodes the streams on
//! workers. The transform parallelises over b-tree groups (see `transform`).

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
    /// Threads (1: everything on the calling thread, as it always was). The output is the
    /// same bytes whatever the number.
    pub threads: usize,
}

impl Default for Options {
    fn default() -> Self {
        Options { level: 7, level_bytes: 9, verify: true, ldm: true, catch: true, threads: 1 }
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
        std::panic::catch_unwind(|| transform::encode_threads(src, opt.threads)).ok().flatten()
    } else {
        transform::encode_threads(src, opt.threads)
    };
    st.transform_s += t.elapsed().as_secs_f64();
    crate::mem::mark("encode");
    let Some(enc) = enc else {
        st.whole = true;
        return whole(src, opt.level);
    };
    if opt.threads > 1 {
        return compress_par(src, opt, st, enc);
    }
    if opt.verify {
        // zstd is trusted: rebuild the file from the streams (before compressing them, so
        // that the streams can go as they are compressed).
        let t = std::time::Instant::now();
        let ok = transform::verify(&enc.geom, src, &enc.streams);
        st.verify_s += t.elapsed().as_secs_f64();
        crate::mem::mark("verify");
        if !ok {
            st.whole = true;
            return whole(src, opt.level);
        }
    }
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
    let mut z = Vec::new();
    for (i, s) in enc.streams.into_iter().enumerate() {
        put_uv(&mut out, s.len() as u64);
        st.raw[i] += s.len() as u64;
        let c = if i == transform::S_BYTES { &mut czb } else { &mut cz };
        z.clear();
        if s.len() > 64 {
            z.reserve(zstd::zstd_safe::compress_bound(s.len()));
            c.compress_to_buffer(&s, &mut z).expect("zstd");
        }
        if !z.is_empty() && z.len() < s.len() {
            out.push(1);
            put_uv(&mut out, z.len() as u64);
            out.extend_from_slice(&z);
            st.coded[i] += z.len() as u64;
        } else {
            out.push(0);
            put_uv(&mut out, s.len() as u64);
            out.extend_from_slice(&s);
            st.coded[i] += s.len() as u64;
        }
    }
    st.zstd_s += t.elapsed().as_secs_f64();
    crate::mem::mark("zstd");
    out
}

fn compressor(opt: &Options, src_len: usize, level: i32) -> zstd::bulk::Compressor<'static> {
    let mut cz = zstd::bulk::Compressor::new(level).expect("zstd");
    // Long-distance matching for what lies beyond the window.
    if opt.ldm && src_len > 2 << 20 {
        use zstd::zstd_safe::CParameter as P;
        cz.set_parameter(P::EnableLongDistanceMatching(true)).expect("zstd");
        cz.set_parameter(P::WindowLog(27)).expect("zstd");
    }
    cz
}

/// What a worker of `compress_par` did.
enum Done {
    /// Whether the streams rebuild the file, and the seconds.
    Verify(bool, f64),
    /// The compressed stream (`None`: stored), and the seconds.
    Stream(Option<Vec<u8>>, f64),
}

/// `compress_with` on several threads: the same bytes. Verification and the streams are
/// tasks of a queue (the text/blob stream first, being the longest); the streams are only
/// read, so they all stay until the end.
fn compress_par(src: &[u8], opt: &Options, st: &mut Stats, enc: transform::Encoded) -> Vec<u8> {
    use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
    const VERIFY: usize = NSTREAMS;
    let (g, streams) = (enc.geom, enc.streams);
    let mut order = vec![transform::S_BYTES];
    if opt.verify {
        order.push(VERIFY);
    }
    let mut rest: Vec<usize> = (0..NSTREAMS).filter(|&i| i != transform::S_BYTES).collect();
    rest.sort_by_key(|&i| std::cmp::Reverse(streams[i].len()));
    order.extend(rest);
    let bad = AtomicBool::new(false);
    let done = crate::par::map(
        opt.threads,
        NSTREAMS + 1,
        &order,
        || (None, None),
        |cs: &mut (Option<zstd::bulk::Compressor>, Option<zstd::bulk::Compressor>), i| {
            let t = std::time::Instant::now();
            if i == VERIFY {
                let run = || transform::verify(&g, src, &streams);
                let ok = if opt.catch {
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(run)).unwrap_or(false)
                } else {
                    run()
                };
                if !ok {
                    bad.store(true, Relaxed);
                }
                return Done::Verify(ok, t.elapsed().as_secs_f64());
            }
            let s = &streams[i];
            if s.len() <= 64 || bad.load(Relaxed) {
                return Done::Stream(None, 0.0);
            }
            let (slot, level) =
                if i == transform::S_BYTES { (&mut cs.1, opt.level_bytes) } else { (&mut cs.0, opt.level) };
            let c = slot.get_or_insert_with(|| compressor(opt, src.len(), level));
            let mut z = Vec::with_capacity(zstd::zstd_safe::compress_bound(s.len()));
            c.compress_to_buffer(s.as_slice(), &mut z).expect("zstd");
            z.shrink_to_fit();
            Done::Stream((!z.is_empty() && z.len() < s.len()).then_some(z), t.elapsed().as_secs_f64())
        },
    );
    let mut zs = Vec::with_capacity(NSTREAMS);
    for (i, d) in done.into_iter().enumerate() {
        match d {
            Some(Done::Verify(ok, secs)) => {
                st.verify_s += secs;
                if !ok {
                    st.whole = true;
                    return whole(src, opt.level);
                }
            }
            Some(Done::Stream(z, secs)) => {
                st.zstd_s += secs;
                zs.push(z);
            }
            None => assert!(i == VERIFY && !opt.verify),
        }
    }
    crate::mem::mark("verify, zstd");
    let mut out = Vec::new();
    header(&mut out, 1, src);
    put_uv(&mut out, g.psize as u64);
    put_uv(&mut out, g.usable as u64);
    out.push(g.fmt4 as u8);
    for (i, (s, z)) in streams.iter().zip(zs).enumerate() {
        put_uv(&mut out, s.len() as u64);
        st.raw[i] += s.len() as u64;
        let c: &[u8] = match &z {
            Some(z) => {
                out.push(1);
                z
            }
            None => {
                out.push(0);
                s
            }
        };
        put_uv(&mut out, c.len() as u64);
        out.extend_from_slice(c);
        st.coded[i] += c.len() as u64;
    }
    out
}

pub fn decompress(src: &[u8]) -> Result<Vec<u8>> {
    decompress_threads(src, 1)
}

/// `decompress` with `threads` threads: the same result.
pub fn decompress_threads(src: &[u8], threads: usize) -> Result<Vec<u8>> {
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
            if threads > 1 {
                let streams = decode_streams_par(&mut r, len, threads)?;
                crate::mem::mark("dec: streams");
                let out = transform::decode_threads(&g, len, &streams, threads)?;
                return if out.len() != len || xxh3_64(&out) != hash {
                    Err(crate::Error("checksum mismatch"))
                } else {
                    Ok(out)
                };
            }
            let mut dz = decompressor()?;
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
            crate::mem::mark("dec: streams");
            transform::decode(&g, len, &streams)?
        }
        _ => return Err(corrupt()),
    };
    if out.len() != len || xxh3_64(&out) != hash {
        return Err(crate::Error("checksum mismatch"));
    }
    Ok(out)
}

fn decompressor() -> Result<zstd::bulk::Decompressor<'static>> {
    let mut dz = zstd::bulk::Decompressor::new().map_err(|_| corrupt())?;
    dz.set_parameter(zstd::zstd_safe::DParameter::WindowLogMax(27)).map_err(|_| corrupt())?;
    Ok(dz)
}

/// The streams of a container, decoded on `threads` threads (the headers are parsed first,
/// in order, with the same checks).
fn decode_streams_par(r: &mut Rd, len: usize, threads: usize) -> Result<Vec<Vec<u8>>> {
    let mut parts = Vec::with_capacity(NSTREAMS);
    for _ in 0..NSTREAMS {
        let n = r.usize()?;
        let m = r.u8()?;
        let c = r.usize()?;
        let d = r.bytes(c)?;
        match m {
            0 if c == n => {}
            1 if n <= len.saturating_mul(2) + (1 << 20) => {}
            _ => return Err(corrupt()),
        }
        parts.push((n, m, d));
    }
    let mut order: Vec<usize> = (0..NSTREAMS).collect();
    order.sort_by_key(|&i| std::cmp::Reverse(parts[i].0));
    let res = crate::par::map(threads, NSTREAMS, &order, decompressor, |dz, i| {
        let (n, m, d) = parts[i];
        if m == 0 {
            return Ok(d.to_vec());
        }
        let mut v = Vec::with_capacity(n);
        dz.as_mut().map_err(|e| e.clone())?.decompress_to_buffer(d, &mut v).map_err(|_| corrupt())?;
        if v.len() != n {
            return Err(corrupt());
        }
        Ok(v)
    });
    res.into_iter().map(|x| x.unwrap()).collect()
}
