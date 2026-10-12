//! The file format, and compression / decompression of whole files.
//!
//! A PNG file's pixel data comes in *images*: the run of IDAT chunks (the default image),
//! and in an APNG every run of fdAT chunks (a frame, whose size its fcTL gives; each fdAT
//! starts with a sequence number). An image's *payload* is the concatenation of its chunks'
//! data (past the sequence numbers): one or more zlib streams (several in Apple's iDOT
//! files). Every image is coded on its own, or kept as it is if coding does not make it
//! smaller.
//!
//! See FORMAT.md for the full specification.
//!
//! ```text
//! magic 69 04 7f a7 59 e0 ("aQR/p1ng" in base64), version(u16 LE) = 0, mode(u8)
//! mode 0 (stored): the input verbatim (when coding does not make it smaller)
//! mode 1 (coded):
//!   u64 LE xxh3 of the original (checked after decompression)
//!   varint length + zstd(skeleton)   -- the chunks without image data, CRC exceptions, trailer
//!   per image: u32 LE length + record
//! record:
//!   0, payload                       -- kept as it is
//!   1, varint length + layout        -- streams, blocks, exceptions, model kind
//!      varint length + headers       -- dynamic Huffman headers (flate-recons' header coder)
//!      model payload                 -- see model::Recorder::finish
//! ```

use crate::model::{self, Geometry, Kind, Span};
use crate::png::{self, Ihdr};
use crate::rans::{BitR, BitW};
use crate::{Error, Result};
use flate_recons::bits::{BitReader, BitWriter};
use flate_recons::deflate::{Block, Body, Stream, Token};
use flate_recons::header;
use recons_common::varint;

const MAGIC: [u8; 6] = [0x69, 0x04, 0x7f, 0xa7, 0x59, 0xe0];
const VERSION: u16 = 0;
/// The magic, the version and the mode.
pub const HEADER_LEN: usize = 9;

/// Appends the file header with mode `mode`.
fn put_header(out: &mut Vec<u8>, mode: u8) {
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.push(mode);
}

pub fn put_varint(out: &mut Vec<u8>, v: u64) {
    varint::put(out, v)
}

pub fn get_varint(src: &[u8], at: &mut usize) -> Result<u64> {
    varint::get(src, at).map_err(|_| crate::corrupt())
}

fn get_bytes<'a>(src: &'a [u8], at: &mut usize, n: usize) -> Result<&'a [u8]> {
    let s = src.get(*at..at.checked_add(n).ok_or_else(crate::corrupt)?).ok_or_else(crate::corrupt)?;
    *at += n;
    Ok(s)
}

fn chunk_crc(kind: &[u8; 4], data: &[u8]) -> u32 {
    let mut h = crc32fast::Hasher::new();
    h.update(kind);
    h.update(data);
    h.finalize()
}

/// Times of the compression steps, for benchmarking.
#[derive(Debug, Default, Clone, Copy)]
pub struct Phases {
    /// The chunk layer (skeleton).
    pub parse: f64,
    /// Parsing and modelling the DEFLATE streams.
    pub model: f64,
    /// The block layout and Huffman headers.
    pub layout: f64,
    pub entropy: f64,
    pub write: f64,
}

/// Compresses a PNG file; anything that cannot be coded is stored.
pub fn compress(file: &[u8]) -> Vec<u8> {
    compress_timed(file, &mut Phases::default())
}

pub fn compress_timed(file: &[u8], ph: &mut Phases) -> Vec<u8> {
    compress_timed_threads(file, ph, 1)
}

/// [`compress`] on up to `threads` threads: the same bytes whatever `threads` is, and
/// `threads == 1` is [`compress`]. Several images are coded at once; a single large image
/// has its symbols computed and counted by several threads.
pub fn compress_threads(file: &[u8], threads: usize) -> Vec<u8> {
    compress_timed_threads(file, &mut Phases::default(), threads)
}

fn compress_timed_threads(file: &[u8], ph: &mut Phases, threads: usize) -> Vec<u8> {
    match std::panic::catch_unwind(|| {
        let mut ph2 = Phases::default();
        let r = coded(file, &mut ph2, threads);
        (r, ph2)
    }) {
        // Coding must save something over storing.
        Ok((Ok(v), p)) if v.len() < file.len() + HEADER_LEN => {
            *ph = p;
            v
        }
        _ => stored(file),
    }
}

/// [`compress`] without the guard against panics (for tests: a panic is a bug even where
/// storing the file would hide it).
pub fn try_compress(file: &[u8]) -> Vec<u8> {
    match coded(file, &mut Phases::default(), 1) {
        Ok(v) if v.len() < file.len() + HEADER_LEN => v,
        _ => stored(file),
    }
}

fn stored(file: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(file.len() + HEADER_LEN);
    put_header(&mut out, 0);
    out.extend_from_slice(file);
    out
}

/// How a chunk's data is kept in the skeleton.
const PLAIN: u8 = 0;
/// IDAT: all of the data is image payload.
const IDAT: u8 = 1;
/// fdAT: a sequence number (in the skeleton), then image payload.
const FDAT: u8 = 2;

/// The images of a chunk list: runs of chunks of the same image class, with the geometry
/// their payload has (the IHDR's, or the frame's from the last fcTL), if valid.
fn images<'a>(
    classes: &[u8],
    data: impl Fn(usize) -> &'a [u8],
    kinds: impl Fn(usize) -> [u8; 4],
) -> Vec<(std::ops::Range<usize>, Option<Ihdr>)> {
    let ihdr = if classes.first() == Some(&PLAIN) && &kinds(0) == b"IHDR" { Ihdr::parse(data(0)) } else { None };
    let mut frame: Option<Ihdr> = None;
    let mut out = vec![];
    let mut i = 0;
    while i < classes.len() {
        let c = classes[i];
        if c == PLAIN {
            if &kinds(i) == b"fcTL" {
                let d = data(i);
                frame = match (ihdr, d.len()) {
                    (Some(h), 26) => Some(Ihdr {
                        width: u32::from_be_bytes(d[4..8].try_into().unwrap()),
                        height: u32::from_be_bytes(d[8..12].try_into().unwrap()),
                        ..h
                    }),
                    _ => None,
                };
            }
            i += 1;
            continue;
        }
        let start = i;
        while i < classes.len() && classes[i] == c {
            i += 1;
        }
        out.push((start..i, if c == IDAT { ihdr } else { frame }));
    }
    out
}

fn coded(file: &[u8], ph: &mut Phases, threads: usize) -> Result<Vec<u8>> {
    let t0 = std::time::Instant::now();
    let (chunks, trailer) = png::chunks(file).ok_or(Error::Unsupported("not a PNG file"))?;
    let classes: Vec<u8> = chunks
        .iter()
        .map(|c| match &c.kind {
            b"IDAT" => IDAT,
            b"fdAT" if c.data.len() >= 4 => FDAT,
            _ => PLAIN,
        })
        .collect();

    // Skeleton: everything but the image payloads.
    let mut sk = vec![];
    put_varint(&mut sk, chunks.len() as u64);
    for (c, &class) in chunks.iter().zip(&classes) {
        sk.extend_from_slice(&c.kind);
        put_varint(&mut sk, c.data.len() as u64);
        if chunk_crc(&c.kind, c.data) == c.crc {
            sk.push(class);
        } else {
            sk.push(class | 0x80);
            sk.extend_from_slice(&c.crc.to_be_bytes());
        }
        match class {
            PLAIN => sk.extend_from_slice(c.data),
            FDAT => sk.extend_from_slice(&c.data[..4]),
            _ => {}
        }
    }
    put_varint(&mut sk, trailer.len() as u64);
    sk.extend_from_slice(trailer);
    ph.parse += t0.elapsed().as_secs_f64();

    // Below this size there is nothing worth a thread to overlap.
    if threads > 1 && file.len() >= PAR_MIN {
        return Ok(coded_threads(file, &chunks, &classes, &sk, threads));
    }
    let z = zstd::bulk::compress(&sk, 19).expect("zstd");
    // Records are kept only if smaller than their payload: the output stays within this.
    let mut out = Vec::with_capacity(file.len() + z.len() + 64);
    put_header(&mut out, 1);
    out.extend_from_slice(&xxhash_rust::xxh3::xxh3_64(file).to_le_bytes());
    put_varint(&mut out, z.len() as u64);
    out.extend_from_slice(&z);
    for (range, ih) in images(&classes, |i| chunks[i].data, |i| chunks[i].kind) {
        let skip = if classes[range.start] == FDAT { 4 } else { 0 };
        let mut payload = Vec::with_capacity(chunks[range.clone()].iter().map(|c| c.data.len()).sum());
        for c in &chunks[range.clone()] {
            payload.extend_from_slice(&c.data[skip..]);
        }
        // The record goes straight into the output, after its length.
        let at = out.len();
        out.extend_from_slice(&[0; 4]);
        let coded = ih.is_some_and(|ih| image_record(&ih, &payload, ph, 1, &mut out).is_ok())
            && out.len() - at - 4 <= payload.len();
        if !coded {
            out.truncate(at + 4);
            out.push(0);
            out.extend_from_slice(&payload);
        }
        let len = (out.len() - at - 4) as u32;
        out[at..at + 4].copy_from_slice(&len.to_le_bytes());
        if std::env::var_os("PNGR_DEBUG").is_some() {
            eprintln!(
                "image {} payload {} -> record {len} ({})",
                classes[range.start],
                payload.len(),
                if coded { "coded" } else { "kept" }
            );
        }
    }
    Ok(out)
}

/// Files smaller than this are handled on one thread whatever the thread count.
const PAR_MIN: usize = 1 << 16;

/// Runs `f(0..n)` on `workers` threads (the caller's among them), each index once, and
/// returns the results in order.
fn par_map<R: Send>(n: usize, workers: usize, f: impl Fn(usize) -> R + Sync) -> Vec<R> {
    let next = std::sync::atomic::AtomicUsize::new(0);
    let out: std::sync::Mutex<Vec<Option<R>>> = std::sync::Mutex::new((0..n).map(|_| None).collect());
    let work = || {
        loop {
            let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if i >= n {
                break;
            }
            let r = f(i);
            out.lock().unwrap()[i] = Some(r);
        }
    };
    std::thread::scope(|s| {
        for _ in 1..workers.min(n) {
            s.spawn(work);
        }
        work();
    });
    out.into_inner().unwrap().into_iter().map(|r| r.expect("worker result")).collect()
}

/// The part of [`coded`] after the skeleton, on `threads` threads: the skeleton's zstd and
/// the checksum beside the images, the images (each into a record of its own) in parallel.
/// The same bytes as the serial code writes.
fn coded_threads(file: &[u8], chunks: &[png::Chunk], classes: &[u8], sk: &[u8], threads: usize) -> Vec<u8> {
    let imgs = images(classes, |i| chunks[i].data, |i| chunks[i].kind);
    let workers = threads.min(imgs.len()).max(1);
    // Threads left over from the images go to each image's own parallel steps.
    let inner = (threads / workers).max(1);
    let (z, hash, recs) = std::thread::scope(|s| {
        let side = s.spawn(|| (zstd::bulk::compress(sk, 19).expect("zstd"), xxhash_rust::xxh3::xxh3_64(file)));
        let recs = par_map(imgs.len(), workers, |i| {
            let (range, ih) = &imgs[i];
            let skip = if classes[range.start] == FDAT { 4 } else { 0 };
            let mut payload = Vec::with_capacity(chunks[range.clone()].iter().map(|c| c.data.len()).sum());
            for c in &chunks[range.clone()] {
                payload.extend_from_slice(&c.data[skip..]);
            }
            let mut rec = vec![0u8; 4];
            let coded = ih
                .as_ref()
                .is_some_and(|ih| image_record(ih, &payload, &mut Phases::default(), inner, &mut rec).is_ok())
                && rec.len() - 4 <= payload.len();
            if !coded {
                rec.truncate(4);
                rec.push(0);
                rec.extend_from_slice(&payload);
            }
            let len = (rec.len() - 4) as u32;
            rec[..4].copy_from_slice(&len.to_le_bytes());
            if std::env::var_os("PNGR_DEBUG").is_some() {
                eprintln!(
                    "image {} payload {} -> record {len} ({})",
                    classes[range.start],
                    payload.len(),
                    if coded { "coded" } else { "kept" }
                );
            }
            rec
        });
        let (z, hash) = side.join().unwrap_or_else(|e| std::panic::resume_unwind(e));
        (z, hash, recs)
    });
    let mut out = Vec::with_capacity(file.len() + z.len() + 64);
    put_header(&mut out, 1);
    out.extend_from_slice(&hash.to_le_bytes());
    put_varint(&mut out, z.len() as u64);
    out.extend_from_slice(&z);
    for r in &recs {
        out.extend_from_slice(r);
    }
    out
}

/// Appends the record of a coded image to `rec`.
fn image_record(ih: &Ihdr, payload: &[u8], ph: &mut Phases, threads: usize, rec: &mut Vec<u8>) -> Result<()> {
    let g = Geometry::new(ih).ok_or(Error::Unsupported("bad image header"))?;
    let kind = if ih.depth == 8 && matches!(ih.color, 0 | 2 | 4 | 6) { Kind::Image } else { Kind::Bytes };
    let t1 = std::time::Instant::now();
    let enc = model::encode_threads(kind, &g, payload, threads)?;
    // The layout codes the data length in 32 bits.
    if enc.len > u32::MAX as usize {
        return Err(Error::Unsupported("image data of 4 GiB or more"));
    }
    if std::env::var_os("PNGR_DEBUG").is_some() {
        eprintln!("  {} zlib streams, {} bytes of data", enc.streams.len(), enc.len);
    }
    let t2 = std::time::Instant::now();
    let t3 = write_record(kind, enc, threads, rec);
    let t4 = std::time::Instant::now();
    ph.model += (t2 - t1).as_secs_f64();
    ph.layout += (t3 - t2).as_secs_f64();
    ph.entropy += (t4 - t3).as_secs_f64();
    Ok(())
}

/// Appends the coded record of `enc` to `rec`; returns when the entropy coding started.
fn write_record(kind: Kind, enc: model::Encoded, threads: usize, rec: &mut Vec<u8>) -> std::time::Instant {
    // Layout.
    let mut lw = BitW::default();
    lw.bits(1, (kind == Kind::Bytes) as u32);
    lw.gamma(enc.len as u32);
    lw.gamma(enc.streams.len() as u32 - 1);
    for c in &enc.streams {
        lw.bits(8, c.zhdr[0] as u32);
        lw.bits(8, c.zhdr[1] as u32);
        let blocks = &c.stream.blocks;
        lw.gamma(blocks.len() as u32);
        let std_last = blocks.iter().enumerate().all(|(i, b)| b.last == (i + 1 == blocks.len()));
        lw.bits(1, std_last as u32);
        let mut prev_count = 0usize;
        for (i, b) in blocks.iter().enumerate() {
            if !std_last {
                lw.bits(1, b.last as u32);
            }
            lw.bits(2, b.body.btype() as u32);
            match b.body {
                Body::Stored { pad, len } => {
                    lw.bits(8, pad as u32);
                    lw.bits(16, len as u32);
                }
                _ => {
                    let count = c.starts[i + 1] - c.starts[i];
                    // The same count as the previous block is common (encoders' buffers).
                    if count == prev_count {
                        lw.bits(1, 1);
                    } else {
                        lw.bits(1, 0);
                        lw.gamma(count as u32);
                    }
                    prev_count = count;
                }
            }
        }
        lw.bits(8, c.stream.tail_pad as u32);
        lw.gamma(c.stream.trailing.len() as u32);
        for &b in &c.stream.trailing {
            lw.bits(8, b as u32);
        }
    }
    // Matches of length 258 sent as 284 + 31.
    lw.gamma(enc.alts.len() as u32);
    let mut last = 0;
    for &i in &enc.alts {
        lw.gamma((i - last) as u32);
        last = i;
    }

    // Headers.
    let mut hw = BitWriter::new();
    let mut recipe = header::Recipe::default();
    for c in &enc.streams {
        for (b, (lf, df)) in c.stream.blocks.iter().zip(&c.freqs) {
            if let Body::Dynamic { header: h, .. } = &b.body {
                header::write(&mut hw, lf, df, h, &mut recipe);
            }
        }
    }
    let pad = hw.pad_len();
    hw.bits(pad, 0);
    let headers = hw.finish();
    let layout = lw.finish();

    let t3 = std::time::Instant::now();
    rec.push(1);
    for part in [&layout, &headers] {
        put_varint(rec, part.len() as u64);
        rec.extend_from_slice(part);
    }
    enc.rec.finish_threads(rec, threads);
    t3
}

thread_local! {
    /// Seconds spent in decompression steps (tables, tokens, headers, writing), for
    /// benchmarking.
    pub static DEC_TIMES: std::cell::Cell<[f64; 4]> = const { std::cell::Cell::new([0.0; 4]) };
}

fn dec_time(i: usize, t: std::time::Instant) -> std::time::Instant {
    let now = std::time::Instant::now();
    DEC_TIMES.with(|c| {
        let mut v = c.get();
        v[i] += (now - t).as_secs_f64();
        c.set(v);
    });
    now
}

pub fn decompress(src: &[u8]) -> Result<Vec<u8>> {
    decompress_threads(src, 1)
}

/// [`decompress`] on up to `threads` threads: the same result whatever `threads` is, and
/// `threads == 1` is [`decompress`]. Several images are rebuilt at once, and a payload
/// of several zlib streams has its streams written at once.
pub fn decompress_threads(src: &[u8], threads: usize) -> Result<Vec<u8>> {
    if src.len() < HEADER_LEN || src[..6] != MAGIC || u16::from_le_bytes([src[6], src[7]]) != VERSION {
        return Err(crate::corrupt());
    }
    match src[8] {
        0 => return Ok(src[HEADER_LEN..].to_vec()),
        1 => {}
        _ => return Err(crate::corrupt()),
    }
    let mut at = HEADER_LEN;
    let hash = u64::from_le_bytes(get_bytes(src, &mut at, 8)?.try_into().unwrap());
    let part = |at: &mut usize| -> Result<&[u8]> {
        let n = get_varint(src, at)? as usize;
        get_bytes(src, at, n)
    };
    let sk = zstd::bulk::decompress(part(&mut at)?, 1 << 30).map_err(|_| crate::corrupt())?;

    // Skeleton.
    let mut k = 0;
    let nchunks = get_varint(&sk, &mut k)? as usize;
    struct C<'a> {
        kind: [u8; 4],
        len: usize,
        crc: Option<u32>,
        /// The whole data (plain chunks) or the sequence number (fdAT).
        data: &'a [u8],
    }
    let mut cs = Vec::with_capacity(nchunks.min(1 << 16));
    let mut classes = Vec::with_capacity(nchunks.min(1 << 16));
    for _ in 0..nchunks {
        let kind: [u8; 4] = get_bytes(&sk, &mut k, 4)?.try_into().unwrap();
        let len = get_varint(&sk, &mut k)? as usize;
        let flags = *sk.get(k).ok_or_else(crate::corrupt)?;
        k += 1;
        let crc = if flags & 0x80 != 0 {
            Some(u32::from_be_bytes(get_bytes(&sk, &mut k, 4)?.try_into().unwrap()))
        } else {
            None
        };
        let class = flags & 0x7f;
        let data = match class {
            PLAIN => get_bytes(&sk, &mut k, len)?,
            FDAT if len >= 4 => get_bytes(&sk, &mut k, 4)?,
            IDAT => &[][..],
            _ => return Err(crate::corrupt()),
        };
        cs.push(C { kind, len, crc, data });
        classes.push(class);
    }
    let tlen = get_varint(&sk, &mut k)? as usize;
    let trailer = get_bytes(&sk, &mut k, tlen)?;
    if k != sk.len() {
        return Err(crate::corrupt());
    }

    // Images.
    let imgs = images(&classes, |i| cs[i].data, |i| cs[i].kind);
    let img_len = |range: &std::ops::Range<usize>| {
        let skip = if classes[range.start] == FDAT { 4 } else { 0 };
        cs[range.clone()].iter().map(|c| c.len - skip).sum::<usize>()
    };
    let payloads: Vec<Vec<u8>> = if threads > 1 && imgs.len() > 1 {
        // The records are length-prefixed: slice them all, rebuild them in parallel (a
        // damaged prefix is reported after the records before it).
        let mut recs = vec![];
        let mut bad = None;
        for _ in &imgs {
            let r = get_bytes(src, &mut at, 4)
                .map(|b| u32::from_le_bytes(b.try_into().unwrap()) as usize)
                .and_then(|n| get_bytes(src, &mut at, n));
            match r {
                Ok(r) => recs.push(r),
                Err(e) => {
                    bad = Some(e);
                    break;
                }
            }
        }
        let workers = threads.min(recs.len()).max(1);
        let inner = (threads / workers).max(1);
        let res = par_map(recs.len(), workers, |i| image_payload(recs[i], img_len(&imgs[i].0), &imgs[i].1, inner));
        let payloads = res.into_iter().collect::<Result<Vec<_>>>()?;
        if let Some(e) = bad {
            return Err(e);
        }
        payloads
    } else {
        let mut payloads = vec![];
        for (range, ih) in &imgs {
            let len = img_len(range);
            let n = u32::from_le_bytes(get_bytes(src, &mut at, 4)?.try_into().unwrap()) as usize;
            let rec = get_bytes(src, &mut at, n)?;
            payloads.push(image_payload(rec, len, ih, threads)?);
        }
        payloads
    };
    if at != src.len() {
        return Err(crate::corrupt());
    }

    // Reassemble the file.
    let total: usize = cs.iter().map(|c| c.len + 12).sum::<usize>() + 8 + trailer.len();
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(&png::SIGNATURE);
    let mut img = 0;
    let mut pa = 0;
    for (i, c) in cs.iter().enumerate() {
        out.extend_from_slice(&(c.len as u32).to_be_bytes());
        out.extend_from_slice(&c.kind);
        let start = out.len();
        match classes[i] {
            PLAIN => out.extend_from_slice(c.data),
            class => {
                while img < imgs.len() && imgs[img].0.end <= i {
                    img += 1;
                    pa = 0;
                }
                if class == FDAT {
                    out.extend_from_slice(c.data);
                }
                let n = c.len - if class == FDAT { 4 } else { 0 };
                let p = payloads.get(img).ok_or_else(crate::corrupt)?;
                out.extend_from_slice(p.get(pa..pa + n).ok_or_else(crate::corrupt)?);
                pa += n;
            }
        }
        let crc = c.crc.unwrap_or_else(|| chunk_crc(&c.kind, &out[start..]));
        out.extend_from_slice(&crc.to_be_bytes());
    }
    out.extend_from_slice(trailer);
    if xxhash_rust::xxh3::xxh3_64(&out) != hash {
        return Err(crate::corrupt());
    }
    Ok(out)
}

/// The payload (of `len` bytes) an image's record gives.
fn image_payload(rec: &[u8], len: usize, ih: &Option<Ihdr>, threads: usize) -> Result<Vec<u8>> {
    let payload = match rec.first() {
        Some(0) if rec.len() == len + 1 => rec[1..].to_vec(),
        Some(1) => {
            let ih = ih.as_ref().ok_or_else(crate::corrupt)?;
            let g = Geometry::new(ih).ok_or_else(crate::corrupt)?;
            rebuild(&g, &rec[1..], threads)?
        }
        _ => return Err(crate::corrupt()),
    };
    if payload.len() != len {
        return Err(crate::corrupt());
    }
    Ok(payload)
}

/// One stream's structure as the layout gives it.
struct StreamLayout {
    zhdr: [u8; 2],
    blocks: Vec<Block>,
    tail_pad: u8,
    trailing: Vec<u8>,
}

/// Rebuilds a coded image's payload from its record (past the mode byte).
fn rebuild(g: &Geometry, rec: &[u8], threads: usize) -> Result<Vec<u8>> {
    let mut at = 0;
    let part = |at: &mut usize| -> Result<&[u8]> {
        let n = get_varint(rec, at)? as usize;
        get_bytes(rec, at, n)
    };
    let layout = part(&mut at)?;
    let headers = part(&mut at)?;
    let mut r = BitR::new(layout);
    let kind = if r.bits(1) == 1 { Kind::Bytes } else { Kind::Image };
    let n = r.gamma()? as usize;
    let nstreams = r.gamma()? as usize + 1;
    if nstreams > layout.len() {
        return Err(crate::corrupt());
    }
    let mut streams = Vec::with_capacity(nstreams);
    let mut spans = vec![];
    for _ in 0..nstreams {
        let zhdr = [r.bits(8) as u8, r.bits(8) as u8];
        let nblocks = r.gamma()? as usize;
        if nblocks > layout.len() * 8 {
            return Err(crate::corrupt());
        }
        let std_last = r.bits(1) == 1;
        let mut blocks = Vec::with_capacity(nblocks);
        let mut prev_count = 0usize;
        for i in 0..nblocks {
            let last = if std_last { i + 1 == nblocks } else { r.bits(1) == 1 };
            let body = match r.bits(2) {
                0 => {
                    let pad = r.bits(8) as u8;
                    let len = r.bits(16) as u16;
                    spans.push(Span::Stored(len as usize));
                    Body::Stored { pad, len }
                }
                t @ (1 | 2) => {
                    let count = if r.bits(1) == 1 { prev_count } else { r.gamma()? as usize };
                    prev_count = count;
                    spans.push(Span::Tokens(count));
                    if t == 1 {
                        Body::Fixed { tokens: vec![] }
                    } else {
                        Body::Dynamic { header: Default::default(), tokens: vec![] }
                    }
                }
                _ => return Err(crate::corrupt()),
            };
            blocks.push(Block { last, body });
            if r.overrun() {
                return Err(crate::corrupt());
            }
        }
        let tail_pad = r.bits(8) as u8;
        let ntrail = r.gamma()? as usize;
        if ntrail > layout.len() * 8 {
            return Err(crate::corrupt());
        }
        let trailing: Vec<u8> = (0..ntrail).map(|_| r.bits(8) as u8).collect();
        streams.push(StreamLayout { zhdr, blocks, tail_pad, trailing });
    }
    let nalts = r.gamma()? as usize;
    if nalts > layout.len() * 8 {
        return Err(crate::corrupt());
    }
    let mut alts = Vec::with_capacity(nalts);
    let mut last = 0usize;
    for _ in 0..nalts {
        last += r.gamma()? as usize;
        alts.push(last);
    }
    if r.overrun() {
        return Err(crate::corrupt());
    }

    let mut buf = vec![0u8; g.pad + n];
    let mut raw = if kind == Kind::Image { vec![0u8; g.raw_len] } else { vec![] };
    let ntok: usize = spans.iter().map(|s| if let Span::Tokens(c) = s { *c } else { 0 }).sum();
    if ntok > n {
        return Err(crate::corrupt());
    }
    let mut tokens = vec![Token::Lit(0); ntok];
    let t = std::time::Instant::now();
    let player = model::Player::new(rec, &mut at)?;
    if at != rec.len() {
        return Err(crate::corrupt());
    }
    let t = dec_time(0, t);
    let mut freqs = Vec::with_capacity(spans.len());
    model::decode(&player, kind, g, &spans, &mut tokens, &mut freqs, &mut buf, &mut raw)?;
    let mut starts = Vec::with_capacity(spans.len() + 1);
    let mut ti = 0;
    for s in &spans {
        starts.push(ti);
        if let Span::Tokens(c) = *s {
            ti += c;
        }
    }
    starts.push(ti);
    for &i in &alts {
        match tokens.get_mut(i) {
            // Listed once only.
            Some(Token::Match { len: 258, alt258: alt258 @ false, .. }) => *alt258 = true,
            _ => return Err(crate::corrupt()),
        }
        // Symbol 284 rather than 285, in the block of token `i`.
        let b = starts.partition_point(|&s| s <= i) - 1;
        let lf = &mut freqs[b].0;
        lf[285] -= 1;
        lf[284] += 1;
    }

    let mut hr = BitReader::new(headers);
    let mut recipe = header::Recipe::default();
    let t = dec_time(1, t);
    let mut bi = 0;
    for s in &mut streams {
        for b in &mut s.blocks {
            if let Body::Dynamic { header: h, .. } = &mut b.body {
                *h = header::read_freqs(&mut hr, &freqs[bi].0, &freqs[bi].1, &mut recipe)?;
            }
            bi += 1;
        }
    }
    let t = dec_time(2, t);

    // Each stream with its blocks' tokens and its part of the data.
    let data = &buf[g.pad..];
    let mut out = Vec::with_capacity(n / 2);
    let (mut b0, mut pos) = (0, 0);
    let single = streams.len() == 1;
    if threads > 1 && !single && n >= PAR_MIN {
        // The streams are written independently: slice them all, write them in parallel.
        struct Job<'a> {
            zhdr: [u8; 2],
            stream: Stream,
            local: Vec<usize>,
            tokens: &'a [Token],
            d: &'a [u8],
        }
        let mut jobs = vec![];
        let mut bad = None;
        for s in streams {
            let nb = s.blocks.len();
            let (t0, t1) = (starts[b0], starts[b0 + nb]);
            let local: Vec<usize> = starts[b0..=b0 + nb].iter().map(|&x| x - t0).collect();
            let stored: usize =
                s.blocks.iter().map(|b| if let Body::Stored { len, .. } = b.body { len as usize } else { 0 }).sum();
            let bytes = stored + tokens[t0..t1].iter().map(|t| t.span()).sum::<usize>();
            let Some(d) = data.get(pos..pos + bytes) else {
                bad = Some(crate::corrupt());
                break;
            };
            let stream = Stream { blocks: s.blocks, tail_pad: s.tail_pad, trailing: s.trailing };
            jobs.push(std::sync::Mutex::new(Some(Job { zhdr: s.zhdr, stream, local, tokens: &tokens[t0..t1], d })));
            b0 += nb;
            pos += bytes;
        }
        let workers = threads.min(jobs.len());
        let res = par_map(jobs.len(), workers, |i| {
            let j = jobs[i].lock().unwrap().take().unwrap();
            flate_recons::write::write(&j.stream, &j.local, j.tokens, j.d).map(|w| (j.zhdr, w))
        });
        for r in res {
            let (zhdr, w) = r?;
            out.extend_from_slice(&zhdr);
            out.extend_from_slice(&w);
        }
        if let Some(e) = bad {
            return Err(e);
        }
    } else {
        for s in streams {
            let nb = s.blocks.len();
            let (t0, t1) = (starts[b0], starts[b0 + nb]);
            let local: Vec<usize> = starts[b0..=b0 + nb].iter().map(|&x| x - t0).collect();
            let bytes = if single {
                n
            } else {
                let stored: usize =
                    s.blocks.iter().map(|b| if let Body::Stored { len, .. } = b.body { len as usize } else { 0 }).sum();
                stored + tokens[t0..t1].iter().map(|t| t.span()).sum::<usize>()
            };
            let d = data.get(pos..pos + bytes).ok_or_else(crate::corrupt)?;
            out.extend_from_slice(&s.zhdr);
            let stream = Stream { blocks: s.blocks, tail_pad: s.tail_pad, trailing: s.trailing };
            out.extend_from_slice(&flate_recons::write::write(&stream, &local, &tokens[t0..t1], d)?);
            b0 += nb;
            pos += bytes;
        }
    }
    if pos != n {
        return Err(crate::corrupt());
    }
    dec_time(3, t);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A token index listed twice among the matches sent as 284 + 31 is corrupt data,
    /// not a counter underflow.
    #[test]
    fn duplicate_alt258() {
        use flate_recons::deflate::{Stream, Token};
        let ih = Ihdr { width: 300, height: 1, depth: 8, color: 0, compression: 0, filter: 0, interlace: 0 };
        let mut data = vec![0u8, 5];
        data.resize(260, 5);
        data.extend((0..41).map(|i| i as u8));
        let mut tokens = vec![Token::Lit(0), Token::Lit(5), Token::Match { len: 258, dist: 1, alt258: true }];
        tokens.extend(data[260..].iter().map(|&b| Token::Lit(b)));
        let stream = Stream { blocks: vec![Block { last: true, body: Body::Fixed { tokens } }], ..Default::default() };
        let mut payload = vec![0x78, 0x9c];
        payload.extend(flate_recons::deflate::write(&stream, &data).unwrap());
        payload.extend([0; 4]);
        let g = Geometry::new(&ih).unwrap();
        let rec_of = |dup: bool| {
            let mut enc = model::encode(Kind::Image, &g, &payload).unwrap();
            assert_eq!(enc.alts, [2]);
            if dup {
                enc.alts.push(2);
            }
            let mut rec = vec![];
            write_record(Kind::Image, enc, 1, &mut rec);
            rec
        };
        assert!(rebuild(&g, &rec_of(false)[1..], 1).unwrap() == payload);
        assert!(matches!(rebuild(&g, &rec_of(true)[1..], 1), Err(Error::Corrupt)));
    }
}
