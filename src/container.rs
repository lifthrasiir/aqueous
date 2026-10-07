//! The file format, and compression / decompression of whole files.
//!
//! ```text
//! "PNGR" version(u8) mode(u8)
//! mode 0 (stored): the input verbatim
//! mode 1 (coded):
//!   u64 LE xxh3 of the original (checked after decompression)
//!   varint length + zstd(skeleton)   -- chunks without IDAT data, CRC exceptions, trailer
//!   varint length + layout           -- DEFLATE blocks, exceptions, model kind
//!   varint length + headers          -- dynamic Huffman headers (flate-recons' header coder)
//!   model payload                    -- see model::Recorder::finish
//! ```

use crate::model::{self, Geometry, Kind, Span};
use crate::png::{self, Ihdr};
use crate::rans::{BitR, BitW};
use crate::{Error, Result};
use flate_recons::bits::{BitReader, BitWriter};
use flate_recons::deflate::{Block, Body, Stream, Token};
use flate_recons::fast::header;

const MAGIC: &[u8; 4] = b"PNGR";
const VERSION: u8 = 1;

pub fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push(v as u8 | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

pub fn get_varint(src: &[u8], at: &mut usize) -> Result<u64> {
    let mut v = 0u64;
    for shift in (0..64).step_by(7) {
        let b = *src.get(*at).ok_or_else(|| crate::corrupt())?;
        *at += 1;
        v |= ((b & 0x7f) as u64) << shift;
        if b < 0x80 {
            return Ok(v);
        }
    }
    Err(crate::corrupt())
}

fn get_bytes<'a>(src: &'a [u8], at: &mut usize, n: usize) -> Result<&'a [u8]> {
    let s = src.get(*at..at.checked_add(n).ok_or_else(|| crate::corrupt())?).ok_or_else(|| crate::corrupt())?;
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
    /// Parsing and modelling the DEFLATE stream.
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
    match std::panic::catch_unwind(|| {
        let mut ph2 = Phases::default();
        let r = coded(file, &mut ph2);
        (r, ph2)
    }) {
        Ok((Ok(v), p)) => {
            *ph = p;
            v
        }
        _ => stored(file),
    }
}

fn stored(file: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(file.len() + 6);
    out.extend_from_slice(MAGIC);
    out.push(VERSION);
    out.push(0);
    out.extend_from_slice(file);
    out
}

fn coded(file: &[u8], ph: &mut Phases) -> Result<Vec<u8>> {
    let t0 = std::time::Instant::now();
    let (chunks, trailer) = png::chunks(file).ok_or(Error::Unsupported("not a PNG file"))?;
    let ihdr = chunks
        .first()
        .filter(|c| &c.kind == b"IHDR")
        .and_then(|c| Ihdr::parse(c.data))
        .ok_or(Error::Unsupported("no IHDR"))?;
    let g = Geometry::new(&ihdr).ok_or(Error::Unsupported("bad IHDR"))?;

    // Skeleton: everything but the IDAT payload.
    let mut sk = vec![];
    put_varint(&mut sk, chunks.len() as u64);
    let mut idat = Vec::new();
    for c in &chunks {
        sk.extend_from_slice(&c.kind);
        put_varint(&mut sk, c.data.len() as u64);
        if chunk_crc(&c.kind, c.data) == c.crc {
            sk.push(0);
        } else {
            sk.push(1);
            sk.extend_from_slice(&c.crc.to_be_bytes());
        }
        if &c.kind == b"IDAT" {
            idat.extend_from_slice(c.data);
        } else {
            sk.extend_from_slice(c.data);
        }
    }
    put_varint(&mut sk, trailer.len() as u64);
    sk.extend_from_slice(trailer);

    let kind = if ihdr.depth == 8 && matches!(ihdr.color, 0 | 2 | 4 | 6) { Kind::Image } else { Kind::Bytes };
    let t1 = std::time::Instant::now();
    let enc = if idat.len() >= 2 { model::encode(kind, &g, &idat[2..]).ok() } else { None };
    let Some(enc) = enc else {
        // Not a DEFLATE stream we can parse: the IDAT payload goes into the skeleton.
        sk.push(0);
        sk.extend_from_slice(&idat);
        return Ok(finish(file, &sk, &[], &[], &[]));
    };
    sk.push(1);
    sk.extend_from_slice(&idat[..2]);
    let t2 = std::time::Instant::now();
    let f = &enc.layout;
    let n = enc.len;

    // Layout.
    let mut lw = BitW::default();
    lw.bits(1, (kind == Kind::Bytes) as u32);
    lw.gamma(n as u32);
    let blocks = &f.stream.blocks;
    lw.gamma(blocks.len() as u32);
    let std_last = blocks.iter().enumerate().all(|(i, b)| b.last == (i + 1 == blocks.len()));
    lw.bits(1, std_last as u32);
    let mut prev_count = 0usize;
    for (i, b) in blocks.iter().enumerate() {
        if !std_last {
            lw.bits(1, b.last as u32);
        }
        lw.bits(2, b.body.btype() as u32);
        match &b.body {
            Body::Stored { pad, len } => {
                lw.bits(8, *pad as u32);
                lw.bits(16, *len as u32);
            }
            _ => {
                let count = f.starts[i + 1] - f.starts[i];
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
    lw.bits(8, f.stream.tail_pad as u32);
    lw.gamma(f.stream.trailing.len() as u32);
    for &b in &f.stream.trailing {
        lw.bits(8, b as u32);
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
    for (i, b) in blocks.iter().enumerate() {
        if let Body::Dynamic { header: h, .. } = &b.body {
            let (lf, df) = &f.stats.freqs[i];
            header::write(&mut hw, lf, df, h, &mut recipe);
        }
    }
    let pad = hw.pad_len();
    hw.bits(pad, 0);
    let headers = hw.finish();

    let t3 = std::time::Instant::now();
    let mut payload = vec![];
    enc.rec.finish(&mut payload);
    let t4 = std::time::Instant::now();
    let out = finish(file, &sk, &lw.finish(), &headers, &payload);
    let t5 = std::time::Instant::now();
    ph.parse += (t1 - t0).as_secs_f64();
    ph.layout += (t3 - t2).as_secs_f64();
    ph.model += (t2 - t1).as_secs_f64();
    ph.entropy += (t4 - t3).as_secs_f64();
    ph.write += (t5 - t4).as_secs_f64();
    Ok(out)
}

fn finish(file: &[u8], sk: &[u8], layout: &[u8], headers: &[u8], payload: &[u8]) -> Vec<u8> {
    let z = zstd::bulk::compress(sk, 19).expect("zstd");
    let mut out = Vec::with_capacity(z.len() + layout.len() + headers.len() + payload.len() + 32);
    out.extend_from_slice(MAGIC);
    out.push(VERSION);
    out.push(1);
    out.extend_from_slice(&xxhash_rust::xxh3::xxh3_64(file).to_le_bytes());
    for part in [&z[..], layout, headers] {
        put_varint(&mut out, part.len() as u64);
        out.extend_from_slice(part);
    }
    out.extend_from_slice(payload);
    out
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
    if src.len() < 6 || &src[..4] != MAGIC || src[4] != VERSION {
        return Err(crate::corrupt());
    }
    match src[5] {
        0 => return Ok(src[6..].to_vec()),
        1 => {}
        _ => return Err(crate::corrupt()),
    }
    let mut at = 6;
    let hash = u64::from_le_bytes(get_bytes(src, &mut at, 8)?.try_into().unwrap());
    let part = |at: &mut usize| -> Result<&[u8]> {
        let n = get_varint(src, at)? as usize;
        get_bytes(src, at, n)
    };
    let z = part(&mut at)?;
    let layout = part(&mut at)?;
    let headers = part(&mut at)?;
    let sk = zstd::bulk::decompress(z, 1 << 30).map_err(|_| crate::corrupt())?;

    // Skeleton.
    let mut k = 0;
    let nchunks = get_varint(&sk, &mut k)? as usize;
    struct C<'a> {
        kind: [u8; 4],
        len: usize,
        crc: Option<u32>,
        data: &'a [u8],
    }
    let mut cs = Vec::with_capacity(nchunks.min(1 << 16));
    let mut idat_len = 0usize;
    for _ in 0..nchunks {
        let kind: [u8; 4] = get_bytes(&sk, &mut k, 4)?.try_into().unwrap();
        let len = get_varint(&sk, &mut k)? as usize;
        let crc = match *sk.get(k).ok_or_else(|| crate::corrupt())? {
            0 => None,
            _ => Some(u32::from_be_bytes(get_bytes(&sk, &mut { k + 1 }, 4)?.try_into().unwrap())),
        };
        k += if crc.is_some() { 5 } else { 1 };
        let data = if &kind == b"IDAT" {
            idat_len += len;
            &[][..]
        } else {
            get_bytes(&sk, &mut k, len)?
        };
        cs.push(C { kind, len, crc, data });
    }
    let tlen = get_varint(&sk, &mut k)? as usize;
    let trailer = get_bytes(&sk, &mut k, tlen)?;
    let has_stream = *sk.get(k).ok_or_else(|| crate::corrupt())?;
    k += 1;
    let idat: Vec<u8> = if has_stream == 0 {
        get_bytes(&sk, &mut k, idat_len)?.to_vec()
    } else {
        let zh = get_bytes(&sk, &mut k, 2)?;
        let ihdr = cs
            .first()
            .filter(|c| &c.kind == b"IHDR")
            .and_then(|c| Ihdr::parse(c.data))
            .ok_or_else(|| crate::corrupt())?;
        let g = Geometry::new(&ihdr).ok_or_else(|| crate::corrupt())?;
        let mut v = Vec::with_capacity(idat_len);
        v.extend_from_slice(zh);
        v.extend_from_slice(&rebuild_stream(&g, layout, headers, src, &mut at)?);
        v
    };
    if idat.len() != idat_len {
        return Err(crate::corrupt());
    }

    // Reassemble the file.
    let mut out = Vec::with_capacity(idat_len + sk.len() + 64);
    out.extend_from_slice(&png::SIGNATURE);
    let mut ia = 0;
    for c in &cs {
        out.extend_from_slice(&(c.len as u32).to_be_bytes());
        out.extend_from_slice(&c.kind);
        let data = if &c.kind == b"IDAT" {
            let d = &idat[ia..ia + c.len];
            ia += c.len;
            d
        } else {
            c.data
        };
        out.extend_from_slice(data);
        let crc = c.crc.unwrap_or_else(|| chunk_crc(&c.kind, data));
        out.extend_from_slice(&crc.to_be_bytes());
    }
    out.extend_from_slice(trailer);
    if xxhash_rust::xxh3::xxh3_64(&out) != hash {
        return Err(crate::corrupt());
    }
    Ok(out)
}

fn rebuild_stream(g: &Geometry, layout: &[u8], headers: &[u8], src: &[u8], at: &mut usize) -> Result<Vec<u8>> {
    let mut r = BitR::new(layout);
    let kind = if r.bits(1) == 1 { Kind::Bytes } else { Kind::Image };
    let n = r.gamma()? as usize;
    let nblocks = r.gamma()? as usize;
    let std_last = r.bits(1) == 1;
    let mut blocks = Vec::with_capacity(nblocks.min(1 << 20));
    let mut spans = Vec::with_capacity(nblocks.min(1 << 20));
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
    let nalts = r.gamma()? as usize;
    let mut alts = Vec::with_capacity(nalts.min(1 << 20));
    let mut last = 0usize;
    for _ in 0..nalts {
        last += r.gamma()? as usize;
        alts.push(last);
    }
    if r.overrun() || n > 1 << 34 {
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
    let player = model::Player::new(src, at)?;
    let t = dec_time(0, t);
    let mut freqs = Vec::with_capacity(spans.len());
    model::decode(&player, kind, g, &spans, &mut tokens, &mut freqs, &mut buf, &mut raw)?;
    let mut starts = Vec::with_capacity(blocks.len() + 1);
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
            Some(Token::Match { len: 258, alt258, .. }) => *alt258 = true,
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
    for (b, f) in blocks.iter_mut().zip(&freqs) {
        if let Body::Dynamic { header: h, .. } = &mut b.body {
            *h = header::read_freqs(&mut hr, &f.0, &f.1, &mut recipe)?;
        }
    }
    let t = dec_time(2, t);
    let stream = Stream { blocks, tail_pad, trailing };
    let out = flate_recons::fast::write::write(&stream, &starts, &tokens, &buf[g.pad..])?;
    dec_time(3, t);
    Ok(out)
}
