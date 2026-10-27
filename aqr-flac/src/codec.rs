//! Coding and decoding of whole FLAC files.
//!
//! Residuals go through [`aqr_raw::audio::residual`] into one rANS stream and two raw bit streams
//! (by channel parity). Everything else (the bytes before the first frame, junk between
//! frames, frame headers, subframe parameters) goes into small byte streams.

use crate::bits;
use crate::flac::{self, ESCAPE, Frame};
use crate::rans::Tables;
use aqr_raw::audio::par::{self, DecBatch, EncBatch, Item};
#[cfg(feature = "encode")]
pub use aqr_raw::audio::residual::freqs_to_bytes;
pub use aqr_raw::audio::residual::{Coded, Error, Rd, layout_id, put_varint, tables};
use aqr_raw::audio::residual::{LmsEncoder, ResCounter, ResDecoder, ResEncoder, get_subframes, put_subframes};
use std::sync::mpsc::sync_channel;

/// Byte streams.
pub const S_PREFIX: usize = 0;
pub const S_LAYOUT: usize = 1;
pub const S_JUNK: usize = 2;
pub const S_HDR: usize = 3;
pub const S_SUB: usize = 4;
pub const S_WARM: usize = 5;
pub const N_BYTE_STREAMS: usize = 6;
pub const STREAM_NAMES: [&str; N_BYTE_STREAMS + 3] =
    ["prefix", "layout", "junk", "headers", "subframes", "warmup", "rans", "raw0", "raw1"];

/// The coded number of a header (bytes from offset 4) and its length.
fn coded_number(h: &[u8]) -> (u64, usize) {
    let b = h[4];
    let n = (b.leading_ones() as usize).max(1);
    let mut v = if n == 1 { b as u64 } else { (b & (0x7f >> n)) as u64 };
    for &c in &h[5..4 + n] {
        v = v << 6 | (c & 0x3f) as u64;
    }
    (v, n)
}

fn put_coded_number(out: &mut Vec<u8>, v: u64) {
    if v < 0x80 {
        out.push(v as u8);
        return;
    }
    let mut n = 2;
    while n < 7 && v >= 1u64 << (5 * n + 1) {
        n += 1;
    }
    let lead = if n == 7 { 0xfe } else { (0xff00u16 >> n) as u8 };
    out.push(lead | (v >> (6 * (n - 1))) as u8);
    for i in (0..n - 1).rev() {
        out.push(0x80 | ((v >> (6 * i)) & 0x3f) as u8);
    }
}

/// The header expected after `prev` (same fields, next frame / sample number).
fn predict_header(prev: &[u8], prev_block: usize, out: &mut Vec<u8>) {
    out.clear();
    if prev.len() < 5 {
        return;
    }
    let (v, n) = coded_number(prev);
    let step = if prev[1] & 1 != 0 { prev_block as u64 } else { 1 };
    out.extend_from_slice(&prev[..4]);
    put_coded_number(out, v + step);
    out.extend_from_slice(&prev[4 + n..]);
}

/// Cuts a FLAC file into frames and junk: the junk, the frame headers and the subframe
/// parameters go to the byte streams; the frames (with their residuals) are returned.
struct Splitter<'a> {
    input: &'a [u8],
    /// `input` padded (see [`bits::padded`]).
    pd: Vec<u8>,
    bps: u32,
    p: usize,
    /// Frames since the last junk.
    run: u64,
    prev_hdr: Vec<u8>,
    prev_block: usize,
    pred: Vec<u8>,
}

impl<'a> Splitter<'a> {
    /// `None` if `input` has no FLAC stream; else with the bytes before the first frame
    /// in `s`.
    fn new(input: &'a [u8], s: &mut [Vec<u8>]) -> Option<Self> {
        let (si, start) = flac::parse_metadata(input)?;
        s[S_PREFIX].extend_from_slice(&input[..start]);
        Some(Splitter {
            input,
            pd: bits::padded(input),
            bps: si.bps,
            p: start,
            run: 0,
            prev_hdr: Vec::new(),
            prev_block: 0,
            pred: Vec::new(),
        })
    }

    /// Parses the next frame into `f`; false at the end of the input.
    #[inline]
    fn next(&mut self, f: &mut Frame, s: &mut [Vec<u8>]) -> bool {
        let input = self.input;
        while self.p < input.len() {
            let p = self.p;
            let Some(e) = flac::parse_frame(&self.pd, p, input.len(), self.bps, f) else {
                // Junk: up to the next position where a frame parses.
                let mut q = p + 1;
                loop {
                    match memchr::memchr(0xff, &input[q.min(input.len())..]) {
                        Some(i) => q += i,
                        None => {
                            q = input.len();
                            break;
                        }
                    }
                    let mut g = Frame::default();
                    if flac::parse_frame(&self.pd, q, input.len(), self.bps, &mut g).is_some() {
                        break;
                    }
                    q += 1;
                }
                put_varint(&mut s[S_LAYOUT], self.run);
                put_varint(&mut s[S_LAYOUT], (q - p) as u64);
                s[S_JUNK].extend_from_slice(&input[p..q]);
                self.run = 0;
                self.p = q;
                continue;
            };
            self.run += 1;
            self.p = e;
            // Header.
            predict_header(&self.prev_hdr, self.prev_block, &mut self.pred);
            if self.pred == f.header {
                s[S_HDR].push(0);
            } else {
                s[S_HDR].push(f.header.len() as u8);
                s[S_HDR].extend_from_slice(&f.header);
            }
            self.prev_hdr.clone_from(&f.header);
            self.prev_block = f.block_size;
            let [sub, warm] = s.get_disjoint_mut([S_SUB, S_WARM]).unwrap();
            put_subframes(f, sub, warm, true);
            return true;
        }
        false
    }

    fn finish(self, s: &mut [Vec<u8>]) {
        put_varint(&mut s[S_LAYOUT], self.run);
    }
}

/// Encodes a FLAC file; `None` if it has no FLAC stream.
pub fn encode(input: &[u8]) -> Option<Coded> {
    let mut s: Vec<Vec<u8>> = vec![Vec::new(); N_BYTE_STREAMS];
    let mut sp = Splitter::new(input, &mut s)?;
    let mut re = ResEncoder::new(input.len());
    let mut f = Frame::default();
    while sp.next(&mut f, &mut s) {
        re.frame(&mut f);
    }
    sp.finish(&mut s);
    let (words, raw) = re.finish();
    Some(Coded { streams: s, words, raw })
}

/// [`encode`] with `threads` threads: the frames are parsed here, the LMS stage and the
/// coding run on two other threads, frames passed in batches. The same output.
pub fn encode_threads(input: &[u8], threads: usize) -> Option<Coded> {
    if threads <= 1 {
        return encode(input);
    }
    // Three stages, one batch each and one waiting: two on workers of the pool.
    let workers = aqr_common::pool::reserve(2);
    if workers.len() < 2 {
        return encode(input);
    }
    let mut s: Vec<Vec<u8>> = vec![Vec::new(); N_BYTE_STREAMS];
    let mut sp = Splitter::new(input, &mut s)?;
    let nb = 6;
    aqr_common::pool::scope_in(workers, |sc| {
        let (rtx, rrx) = sync_channel::<EncBatch>(nb);
        let (ltx, lrx) = sync_channel::<EncBatch>(nb);
        let (ctx, crx) = sync_channel::<EncBatch>(nb);
        for _ in 0..nb {
            rtx.send(EncBatch::default()).expect("channel");
        }
        sc.spawn(move || {
            let mut le = LmsEncoder::default();
            for mut b in lrx {
                for f in &mut b.frames[..b.n] {
                    le.frame(f);
                }
                if ctx.send(b).is_err() {
                    break;
                }
            }
        });
        let coder = sc.spawn(move || {
            let mut re = ResEncoder::new(input.len());
            for mut b in crx {
                for f in &mut b.frames[..b.n] {
                    re.code(f);
                }
                let _ = rtx.send(b);
            }
            re.finish()
        });
        let mut b = rrx.recv().expect("pipeline");
        let mut samples = 0;
        while sp.next(b.next_frame(), &mut s) {
            let f = &b.frames[b.n];
            samples += f.block_size * f.channels;
            b.n += 1;
            if samples >= par::BATCH_SAMPLES || b.n >= par::BATCH_ITEMS {
                ltx.send(b).expect("pipeline");
                b = rrx.recv().expect("pipeline");
                (b.n, samples) = (0, 0);
            }
        }
        sp.finish(&mut s);
        if b.n > 0 {
            ltx.send(b).expect("pipeline");
        }
        drop(ltx);
        let (words, raw) = coder.join().unwrap_or_else(|p| std::panic::resume_unwind(p));
        Some(Coded { streams: s, words, raw })
    })
}

/// Header state across frames, for decoding.
#[derive(Default)]
struct Headers {
    prev_hdr: Vec<u8>,
    prev_block: usize,
    hbuf: Vec<u8>,
}

impl Headers {
    /// Reads the next frame's header and subframe parameters into `f` (residual vectors
    /// sized).
    #[inline]
    fn frame(&mut self, f: &mut Frame, bps: u32, hdr: &mut Rd, sub: &mut Rd, warm: &mut Rd) -> Result<(), Error> {
        let hbuf = &mut self.hbuf;
        let hl = hdr.byte()? as usize;
        if hl == 0 {
            predict_header(&self.prev_hdr, self.prev_block, hbuf);
        } else {
            hbuf.clear();
            hbuf.extend_from_slice(hdr.bytes(hl)?);
        }
        hbuf.push(flac::crc8(hbuf));
        let (hlen, block, assign, fbps) = flac::parse_header(hbuf, 0, bps).ok_or(Error("bad header"))?;
        hbuf.pop();
        debug_assert_eq!(hlen, hbuf.len());
        f.header.clone_from(hbuf);
        f.block_size = block;
        f.assign = assign;
        f.channels = if assign < 8 { assign as usize + 1 } else { 2 };
        f.bps = fbps;
        self.prev_hdr.clone_from(hbuf);
        self.prev_block = block;
        get_subframes(f, sub, warm, true)
    }
}

/// Escaped partitions must hold their values.
#[inline(always)]
fn check_escapes(f: &Frame) -> Result<(), Error> {
    let block = f.block_size;
    for ch in 0..f.channels {
        let sf = &f.sub[ch];
        if !sf.has_residual() || !sf.params.iter().any(|&k| k & ESCAPE != 0) {
            continue;
        }
        let ps = block >> sf.porder;
        let mut at = 0;
        for (i, &k) in sf.params.iter().enumerate() {
            let n = ps - if i == 0 { sf.order as usize } else { 0 };
            if k & ESCAPE != 0 {
                let nb = (k & 31) as u32;
                for &v in &f.res[ch][at..at + n] {
                    let ok = if nb == 0 { v == 0 } else { nb >= 32 || (v >> (nb - 1)) == 0 || (v >> (nb - 1)) == -1 };
                    if !ok {
                        return Err(Error("escaped residual range"));
                    }
                }
            }
            at += n;
        }
    }
    Ok(())
}

/// Checks the streams and the prefix; the stream info.
fn decode_start(c: &Coded) -> Result<flac::StreamInfo, Error> {
    if c.streams.len() != N_BYTE_STREAMS {
        return Err(Error("stream count"));
    }
    let prefix = &c.streams[S_PREFIX];
    let (si, start) = flac::parse_metadata(prefix).ok_or(Error("metadata"))?;
    if start != prefix.len() {
        return Err(Error("metadata length"));
    }
    Ok(si)
}

/// Decodes into `out` (appending).
pub fn decode(c: &Coded, t: &Tables, total_len: usize, out: &mut Vec<u8>) -> Result<(), Error> {
    if c.streams.len() != N_BYTE_STREAMS {
        return Err(Error("stream count"));
    }
    out.reserve(total_len);
    out.extend_from_slice(&c.streams[S_PREFIX]);
    let si = decode_start(c)?;
    let mut layout = Rd::new(&c.streams[S_LAYOUT]);
    let mut junk = Rd::new(&c.streams[S_JUNK]);
    let mut hdr = Rd::new(&c.streams[S_HDR]);
    let mut sub = Rd::new(&c.streams[S_SUB]);
    let mut warm = Rd::new(&c.streams[S_WARM]);
    let raw_buf = [bits::padded(&c.raw[0]), bits::padded(&c.raw[1])];
    let mut rd = ResDecoder::new(t, &c.words, &raw_buf)?;
    let mut f = Frame::default();
    let mut hs = Headers::default();
    loop {
        let run = layout.varint()?;
        for _ in 0..run {
            if out.len() > total_len {
                return Err(Error("too long"));
            }
            hs.frame(&mut f, si.bps, &mut hdr, &mut sub, &mut warm)?;
            rd.frame(&mut f)?;
            check_escapes(&f)?;
            if !flac::write_frame(&f, out, total_len.saturating_sub(out.len())) {
                return Err(Error("too long"));
            }
        }
        if layout.done() {
            break;
        }
        let n = layout.varint()? as usize;
        out.extend_from_slice(junk.bytes(n)?);
    }
    rd.finish()
}

/// Bits of long Rice codes a frame may take when it is written ahead on the pool; one
/// that takes more is written again in order, with the budget of [`decode`].
const AHEAD_BUDGET: u64 = 1 << 20;

/// How a frame was written on the pool.
enum Wrote {
    /// Up to this offset of the batch's bytes, with these bits of long codes.
    Ok(usize, u64),
    /// Over [`AHEAD_BUDGET`]: to write in order.
    Again,
    Err(Error),
}

/// Frames written by the pool.
#[derive(Default)]
struct Written {
    bytes: Vec<u8>,
    wrote: Vec<Wrote>,
}

fn reset_written(w: &mut Written) {
    w.bytes.clear();
    w.wrote.clear();
}

/// The first stage of [`decode_threads`]: frame headers and subframe parameters, junk.
fn front<'a>(
    e: &mut par::Front<'a, impl par::Pass<'a, Written>, Written>,
    c: &'a Coded,
    bps: u32,
) -> Result<(), par::Stop<'a>> {
    let mut layout = Rd::new(&c.streams[S_LAYOUT]);
    let mut junk = Rd::new(&c.streams[S_JUNK]);
    let mut hdr = Rd::new(&c.streams[S_HDR]);
    let mut sub = Rd::new(&c.streams[S_SUB]);
    let mut warm = Rd::new(&c.streams[S_WARM]);
    let mut hs = Headers::default();
    loop {
        let run = layout.varint()?;
        for _ in 0..run {
            let mut f = e.frame();
            if let Err(err) = hs.frame(&mut f, bps, &mut hdr, &mut sub, &mut warm) {
                e.unused(f);
                return Err(par::Stop::Item(Item::FrameErr(err)));
            }
            let n = f.block_size * f.channels;
            e.push(Item::Frame(f), n)?;
        }
        if layout.done() {
            return Ok(());
        }
        let n = layout.varint()? as usize;
        e.push(Item::Junk(junk.bytes(n)?), 0)?;
    }
}

/// [`decode`] with `threads` threads: a pipeline of the frame parameters, the error
/// decoding, the LMS stage, and a pool writing the frames (escape checks, Rice coding,
/// CRCs), then the frames are appended here in order. The same output and errors.
pub fn decode_threads(c: &Coded, t: &Tables, total_len: usize, out: &mut Vec<u8>, threads: usize) -> Result<(), Error> {
    if threads <= 1 {
        return decode(c, t, total_len, out);
    }
    if c.streams.len() != N_BYTE_STREAMS {
        return Err(Error("stream count"));
    }
    // three stages and a pool of one at least, on workers of the pool
    let workers = aqr_common::pool::reserve((threads - 1).max(4));
    if workers.len() < 4 {
        return decode(c, t, total_len, out);
    }
    let pool = workers.len() - 3;
    out.reserve(total_len);
    out.extend_from_slice(&c.streams[S_PREFIX]);
    let si = decode_start(c)?;
    let raw_buf = [bits::padded(&c.raw[0]), bits::padded(&c.raw[1])];
    let rd = ResDecoder::new(t, &c.words, &raw_buf)?;
    let nb = par::in_flight(threads);
    type B<'a> = DecBatch<'a, Written>;
    aqr_common::pool::scope_in(workers, |sc| {
        let (rtx, rrx) = sync_channel::<B>(nb);
        let (tx1, rx1) = sync_channel::<B>(nb);
        let (tx2, rx2) = sync_channel::<B>(nb);
        let (tx3, rx3) = sync_channel::<B>(nb);
        let (otx, orx) = sync_channel::<Result<B, par::Panic>>(nb);
        for _ in 0..nb {
            rtx.send(B::default()).expect("channel");
        }
        sc.spawn(move || {
            if let Some(mut e) = par::Front::new(par::Chans { recycle: &rrx, tx: &tx1 }, reset_written) {
                let r = front(&mut e, c, si.bps);
                e.finish(r);
            }
        });
        sc.spawn(move || par::errors_stage(rd, rx1, tx2));
        sc.spawn(move || par::lms_stage(rx2, tx3));
        par::pool(sc, pool, rx3, otx, |mut b: B| {
            let w = &mut b.x;
            for it in &b.items {
                let Item::Frame(f) = it else { continue };
                if let Err(e) = check_escapes(f) {
                    w.wrote.push(Wrote::Err(e));
                    break;
                }
                w.wrote.push(match flac::write_frame_bits(f, &mut w.bytes, AHEAD_BUDGET) {
                    Some(left) => Wrote::Ok(w.bytes.len(), AHEAD_BUDGET - left),
                    None => Wrote::Again,
                });
            }
            b
        });
        // Appends the batches in order, with the checks of `decode`.
        let mut ro = par::Reorder::default();
        loop {
            let Ok(b) = orx.recv() else { return Err(Error("pipeline")) };
            let b = par::unwrap(b);
            ro.put(b.idx, b);
            while let Some(b) = ro.pop() {
                let (mut k, mut at) = (0, 0);
                for it in &b.items {
                    match it {
                        Item::Junk(j) => out.extend_from_slice(j),
                        Item::Frame(f) => {
                            if out.len() > total_len {
                                return Err(Error("too long"));
                            }
                            let budget = (total_len.saturating_sub(out.len()) as u64).saturating_mul(8);
                            match &b.x.wrote[k] {
                                Wrote::Ok(end, used) => {
                                    if *used > budget {
                                        return Err(Error("too long"));
                                    }
                                    out.extend_from_slice(&b.x.bytes[at..*end]);
                                    at = *end;
                                }
                                Wrote::Again => {
                                    if flac::write_frame_bits(f, out, budget).is_none() {
                                        return Err(Error("too long"));
                                    }
                                }
                                Wrote::Err(e) => return Err(Error(e.0)),
                            }
                            k += 1;
                        }
                        Item::FrameErr(e) => {
                            if out.len() > total_len {
                                return Err(Error("too long"));
                            }
                            return Err(Error(e.0));
                        }
                        Item::Err(e) => return Err(Error(e.0)),
                    }
                }
                if b.last {
                    return Ok(());
                }
                let _ = rtx.send(b);
            }
        }
    })
}

/// Adds the (context, token) counts of a file's residuals to `counts` (for training).
pub fn count(input: &[u8], counts: &mut [Vec<u64>]) {
    let Some((si, start)) = flac::parse_metadata(input) else { return };
    let pd = bits::padded(input);
    let mut rc = ResCounter::default();
    let mut f = Frame::default();
    let mut p = start;
    while p < input.len() {
        let Some(e) = flac::parse_frame(&pd, p, input.len(), si.bps, &mut f) else {
            p += 1;
            continue;
        };
        p = e;
        rc.frame(&mut f, counts);
    }
}
