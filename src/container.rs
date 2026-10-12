//! The file format (FORMAT.md has it in full).
//!
//! ```text
//! magic(69 04 6c ab 58 be) u16-LE(version) u64-LE(length) u64-LE(xxh3 of the original)
//! version 0 (whole): zstd(file)               -- not a database we can code
//! version 1 (coded): varint(page size) varint(usable size) u8(schema format >= 4)
//!                    per stream (transform::NSTREAMS), in their order:
//!                      varint(n << 1 | z) [varint(raw length) if z] n bytes (zstd if z)
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

const MAGIC: [u8; 6] = [0x69, 0x04, 0x6c, 0xab, 0x58, 0xbe];
/// The original, zstd'd whole.
const V_WHOLE: u16 = 0;
/// The streams of the transform.
const V_CODED: u16 = 1;
/// The largest output of a zstd byte (a 4-byte RLE block makes 128 KiB).
const ZSTD_MAX_RATIO: usize = 1 << 15;

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

fn header(out: &mut Vec<u8>, version: u16, src: &[u8]) {
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&version.to_le_bytes());
    out.extend_from_slice(&(src.len() as u64).to_le_bytes());
    out.extend_from_slice(&xxh3_64(src).to_le_bytes());
}

/// The header of version 1 past the common part.
fn geom(out: &mut Vec<u8>, g: &Geom) {
    put_uv(out, g.psize as u64);
    put_uv(out, g.usable as u64);
    out.push(g.fmt4 as u8);
}

/// A stream: `z` its zstd frame, if smaller.
fn put_stream(out: &mut Vec<u8>, s: &[u8], z: Option<&[u8]>) -> usize {
    match z {
        Some(z) => {
            put_uv(out, (z.len() as u64) << 1 | 1);
            put_uv(out, s.len() as u64);
            out.extend_from_slice(z);
            z.len()
        }
        None => {
            put_uv(out, (s.len() as u64) << 1);
            out.extend_from_slice(s);
            s.len()
        }
    }
}

fn whole(src: &[u8], level: i32) -> Vec<u8> {
    let mut out = Vec::new();
    header(&mut out, V_WHOLE, src);
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
    header(&mut out, V_CODED, src);
    geom(&mut out, &enc.geom);
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
        st.raw[i] += s.len() as u64;
        let c = if i == transform::S_BYTES { &mut czb } else { &mut cz };
        z.clear();
        if s.len() > 64 {
            z.reserve(zstd::zstd_safe::compress_bound(s.len()));
            c.compress_to_buffer(&s, &mut z).expect("zstd");
        }
        let small = !z.is_empty() && z.len() < s.len();
        st.coded[i] += put_stream(&mut out, &s, small.then_some(&z[..])) as u64;
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
    header(&mut out, V_CODED, src);
    geom(&mut out, &g);
    for (i, (s, z)) in streams.iter().zip(zs).enumerate() {
        st.raw[i] += s.len() as u64;
        st.coded[i] += put_stream(&mut out, s, z.as_deref()) as u64;
    }
    out
}

pub fn decompress(src: &[u8]) -> Result<Vec<u8>> {
    decompress_threads(src, 1)
}

/// `decompress` with `threads` threads: the same result.
pub fn decompress_threads(src: &[u8], threads: usize) -> Result<Vec<u8>> {
    let mut r = Rd::new(src);
    if r.bytes(6).ok() != Some(&MAGIC[..]) {
        return Err(crate::Error("not an SQLR file"));
    }
    let version = u16::from_le_bytes(r.bytes(2)?.try_into().unwrap());
    let len = usize::try_from(u64::from_le_bytes(r.bytes(8)?.try_into().unwrap())).map_err(|_| corrupt())?;
    let hash = u64::from_le_bytes(r.bytes(8)?.try_into().unwrap());
    let out = match version {
        V_WHOLE => {
            let z = &src[r.p..];
            if len > z.len().saturating_mul(ZSTD_MAX_RATIO) {
                return Err(corrupt());
            }
            let mut out = Vec::with_capacity(len);
            zstd::stream::copy_decode(z, &mut out).map_err(|_| corrupt())?;
            out
        }
        V_CODED => {
            let psize = r.usize()?;
            let usable = r.usize()?;
            let fmt4 = r.u8()? != 0;
            if !(512..=65536).contains(&psize) || !psize.is_power_of_two() || !(480..=psize).contains(&usable) {
                return Err(corrupt());
            }
            let g = Geom { psize, usable, npages: len / psize, fmt4 };
            let parts = stream_parts(&mut r, len)?;
            let streams = if threads > 1 { decode_streams_par(&parts, threads)? } else { decode_streams(&parts)? };
            crate::mem::mark("dec: streams");
            transform::decode_threads(&g, len, &streams, threads)?
        }
        _ => return Err(crate::Error("unknown version")),
    };
    if out.len() != len || xxh3_64(&out) != hash {
        return Err(crate::Error("checksum mismatch"));
    }
    Ok(out)
}

/// A stream as stored: its length, whether it is zstd'd, and the bytes.
type Part<'a> = (usize, bool, &'a [u8]);

/// The streams of a version 1 file, which must end with them; `len` is the original's
/// length.
fn stream_parts<'a>(r: &mut Rd<'a>, len: usize) -> Result<Vec<Part<'a>>> {
    let mut parts = Vec::with_capacity(NSTREAMS);
    for _ in 0..NSTREAMS {
        let v = r.uv()?;
        let c = usize::try_from(v >> 1).map_err(|_| corrupt())?;
        let z = v & 1 == 1;
        let n = if z { r.usize()? } else { c };
        if z && (n > len.saturating_mul(2) + (1 << 20) || n > c.saturating_mul(ZSTD_MAX_RATIO)) {
            return Err(corrupt());
        }
        parts.push((n, z, r.bytes(c)?));
    }
    if !r.at_end() {
        return Err(corrupt());
    }
    Ok(parts)
}

fn decode_part(dz: &mut zstd::bulk::Decompressor, &(n, z, d): &Part) -> Result<Vec<u8>> {
    if !z {
        return Ok(d.to_vec());
    }
    let mut v = Vec::with_capacity(n);
    dz.decompress_to_buffer(d, &mut v).map_err(|_| corrupt())?;
    if v.len() != n {
        return Err(corrupt());
    }
    Ok(v)
}

fn decode_streams(parts: &[Part]) -> Result<Vec<Vec<u8>>> {
    let mut dz = decompressor()?;
    parts.iter().map(|p| decode_part(&mut dz, p)).collect()
}

fn decompressor() -> Result<zstd::bulk::Decompressor<'static>> {
    let mut dz = zstd::bulk::Decompressor::new().map_err(|_| corrupt())?;
    dz.set_parameter(zstd::zstd_safe::DParameter::WindowLogMax(27)).map_err(|_| corrupt())?;
    Ok(dz)
}

/// The streams of a container, decoded on `threads` threads.
fn decode_streams_par(parts: &[Part], threads: usize) -> Result<Vec<Vec<u8>>> {
    let mut order: Vec<usize> = (0..NSTREAMS).collect();
    order.sort_by_key(|&i| std::cmp::Reverse(parts[i].0));
    let res = crate::par::map(threads, NSTREAMS, &order, decompressor, |dz, i| {
        decode_part(dz.as_mut().map_err(|e| e.clone())?, &parts[i])
    });
    res.into_iter().map(|x| x.unwrap()).collect()
}
