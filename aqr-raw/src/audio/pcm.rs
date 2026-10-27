//! Interleaved PCM samples, as they are in WAV, AIFF and most other containers (which
//! [`crate::container`] takes apart).
//!
//! The samples are cut into blocks, and each block becomes a FLAC-like frame (stereo
//! decorrelation, wasted bits, a quantised LPC predictor per channel) whose residuals go
//! through the same pipeline as FLAC residuals ([`crate::audio::residual`]).

use crate::audio::bits;
use crate::audio::frame::{self, Frame, SUB_CONSTANT, SUB_LPC, SUB_VERBATIM};
use crate::audio::par::{self, DecBatch, EncBatch, Item};
use crate::audio::rans::Tables;
use crate::audio::residual::{
    Coded, Error, LmsEncoder, Rd, ResCounter, ResDecoder, ResEncoder, get_subframes, put_subframes,
};
use std::sync::mpsc::sync_channel;

/// Byte streams: the channel assignment of each block, the subframe parameters, and the
/// samples kept in them (warm-up, constant and verbatim).
const S_HDR: usize = 0;
const S_SUB: usize = 1;
const S_WARM: usize = 2;
pub const N_BYTE_STREAMS: usize = 3;

/// Samples per block.
const BLOCK: usize = 4096;
/// Highest LPC order and coefficient precision.
const MAX_ORDER: usize = 16;
const PRECISION: u32 = 14;
/// Weight of the parameter cost per order in the order choice (the LMS stage gains some of
/// what a higher order would).
const ORDER_COST: f64 = 4.0;

/// Layout of the samples.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Format {
    pub channels: usize,
    /// Bytes per sample (1..=4).
    pub bytes: usize,
    pub big_endian: bool,
    /// Unsigned samples (offset binary), as in 8-bit WAV.
    pub unsigned: bool,
}

impl Format {
    pub fn valid(&self) -> bool {
        (1..=frame::MAX_CHANNELS).contains(&self.channels) && (1..=4).contains(&self.bytes)
    }

    /// Parses "s16le", "u8", "s24be", ... with a channel count: "s16le:2".
    pub fn parse(s: &str) -> Option<Format> {
        let (f, ch) = s.split_once(':').unwrap_or((s, "2"));
        let unsigned = match f.as_bytes().first()? {
            b's' => false,
            b'u' => true,
            _ => return None,
        };
        let (bits, end) = if let Some(b) = f[1..].strip_suffix("le") {
            (b, false)
        } else if let Some(b) = f[1..].strip_suffix("be") {
            (b, true)
        } else {
            (&f[1..], false)
        };
        let bits: usize = bits.parse().ok()?;
        let fmt = Format { channels: ch.parse().ok()?, bytes: bits / 8, big_endian: end, unsigned };
        (bits.is_multiple_of(8) && fmt.valid()).then_some(fmt)
    }

    /// Bytes of a sample frame (one sample of each channel).
    pub fn frame_bytes(&self) -> usize {
        self.channels * self.bytes
    }

    /// The format as 3 bytes: channels, bytes per sample, flags (bit 0 big endian, bit 1
    /// unsigned).
    pub fn to_bytes(self) -> [u8; 3] {
        [self.channels as u8, self.bytes as u8, self.big_endian as u8 | (self.unsigned as u8) << 1]
    }

    pub fn from_bytes(b: &[u8]) -> Option<Format> {
        let b: &[u8; 3] = b.try_into().ok()?;
        let f = Format {
            channels: b[0] as usize,
            bytes: b[1] as usize,
            big_endian: b[2] & 1 != 0,
            unsigned: b[2] & 2 != 0,
        };
        (f.valid() && b[2] < 4).then_some(f)
    }

    #[inline(always)]
    fn read(&self, d: &[u8]) -> i64 {
        let mut v = 0u32;
        for i in 0..self.bytes {
            let b = if self.big_endian { d[i] } else { d[self.bytes - 1 - i] };
            v = v << 8 | b as u32;
        }
        let bits = 8 * self.bytes as u32;
        if self.unsigned {
            v ^= 1 << (bits - 1);
        }
        ((v << (32 - bits)) as i32 >> (32 - bits)) as i64
    }

    #[inline(always)]
    fn write(&self, v: i64, out: &mut [u8]) {
        let bits = 8 * self.bytes as u32;
        let mut v = v as u32;
        if self.unsigned {
            v ^= 1 << (bits - 1);
        }
        for i in 0..self.bytes {
            let b = (v >> (8 * i)) as u8;
            if self.big_endian {
                out[self.bytes - 1 - i] = b;
            } else {
                out[i] = b;
            }
        }
    }
}

/// Window for the autocorrelation (Tukey, 0.5), by block size.
fn window(n: usize) -> Vec<f64> {
    let t = (n / 4).max(1);
    (0..n)
        .map(|i| {
            let k = i.min(n - 1 - i);
            if k < t { 0.5 - 0.5 * (std::f64::consts::PI * k as f64 / t as f64).cos() } else { 1.0 }
        })
        .collect()
}

/// Autocorrelation of `w` at lags 0..=`max_lag` (at most 32), with independent partial
/// sums so that it vectorises.
fn autocorr(w: &[f64], max_lag: usize) -> [f64; 33] {
    let mut r = [0f64; 33];
    for (lag, rl) in r.iter_mut().enumerate().take(max_lag + 1) {
        let (a, b) = (&w[lag..], &w[..w.len() - lag]);
        let mut acc = [0f64; 8];
        let (ca, cb) = (a.chunks_exact(8), b.chunks_exact(8));
        let (ra, rb) = (ca.remainder(), cb.remainder());
        for (x, y) in ca.zip(cb) {
            for k in 0..8 {
                acc[k] += x[k] * y[k];
            }
        }
        *rl = acc.iter().sum::<f64>() + ra.iter().zip(rb).map(|(x, y)| x * y).sum::<f64>();
    }
    r
}

/// Quantised LPC coefficients (FLAC's convention: `coefs[j]` weighs sample t-1-j), shift
/// and order for `x` (of `bits` per sample), or `None` if the signal is too small for a
/// predictor to help. The order minimises the estimated size: half the log of the
/// prediction error per sample, plus the coefficients and warm-up samples.
fn lpc(x: &[i64], win: &[f64], bits: u32) -> Option<([i32; 32], i8, usize)> {
    let n = x.len();
    let max_order = MAX_ORDER.min(n / 4);
    if max_order == 0 {
        return None;
    }
    let w: Vec<f64> = x.iter().zip(win).map(|(&v, &w)| v as f64 * w).collect();
    let mut r = autocorr(&w, max_order);
    if r[0] <= 0.0 {
        return None;
    }
    r[0] *= 1.0 + 1e-9;
    // Levinson-Durbin, keeping the coefficients of the best order so far.
    let mut a = [0f64; 33];
    let mut best = (f64::MAX, 0, a);
    let mut err = r[0];
    let per_order = (PRECISION + bits) as f64 * ORDER_COST;
    for i in 0..max_order {
        let mut k = -r[i + 1];
        for j in 0..i {
            k -= a[j] * r[i - j];
        }
        k /= err;
        let prev = a;
        for j in 0..i {
            a[j] = prev[j] + k * prev[i - 1 - j];
        }
        a[i] = k;
        err *= 1.0 - k * k;
        if err <= 0.0 {
            break;
        }
        let est = 0.5 * n as f64 * err.log2() + per_order * (i + 1) as f64;
        if est < best.0 {
            best = (est, i + 1, a);
        }
    }
    let (_, order, a) = best;
    if order == 0 {
        return None;
    }
    // Predictor coefficients are -a; quantise with error feedback.
    let cmax = a[..order].iter().fold(0f64, |m, &v| m.max(v.abs()));
    if cmax <= 0.0 {
        return None;
    }
    let log2 = cmax.log2().floor() as i32 + 1;
    let shift = (PRECISION as i32 - 1 - log2).clamp(0, 15);
    let lim = (1i64 << (PRECISION - 1)) - 1;
    let mut q = [0i32; 32];
    let mut e = 0f64;
    for j in 0..order {
        let v = -a[j] * (1u64 << shift) as f64 + e;
        let r = (v.round() as i64).clamp(-lim - 1, lim);
        e = v - r as f64;
        q[j] = r as i32;
    }
    Some((q, shift as i8, order))
}

/// Sets subframe `ch` of `f` from the samples `x` (already decorrelated), with its
/// residuals.
fn subframe(f: &mut Frame, ch: usize, x: &mut [i64], full_bits: u32, win: &[f64]) {
    let n = x.len();
    let sf = &mut f.sub[ch];
    let res = &mut f.res[ch];
    res.clear();
    sf.params.clear();
    // Wasted bits.
    let or = x.iter().fold(0i64, |a, &v| a | v);
    let wasted = if or == 0 { 0 } else { or.trailing_zeros().min(full_bits - 1) };
    sf.wasted = wasted as u8;
    sf.bits = (full_bits - wasted) as u8;
    if wasted > 0 {
        for v in x.iter_mut() {
            *v >>= wasted;
        }
    }
    if x.iter().all(|&v| v == x[0]) {
        sf.typ = SUB_CONSTANT;
        sf.wasted = 0;
        sf.bits = full_bits as u8;
        sf.order = 0;
        sf.warm[0] = x[0] << wasted;
        return;
    }
    let ok = if let Some((q, shift, order)) = lpc(x, win, full_bits - wasted) {
        sf.typ = SUB_LPC + order as u8 - 1;
        sf.order = order as u8;
        sf.precision = PRECISION as u8;
        sf.shift = shift;
        sf.coefs = q;
        sf.warm[..order].copy_from_slice(&x[..order]);
        residual_lpc(x, &q[..order], shift as u32, res)
    } else {
        // Fixed order 1 (silence, tiny signals).
        sf.typ = frame::SUB_FIXED + 1;
        sf.order = 1;
        sf.warm[0] = x[0];
        let mut ok = true;
        for t in 1..n {
            let e = x[t] - x[t - 1];
            ok &= e as i32 as i64 == e;
            res.push(e as i32);
        }
        ok
    };
    if !ok {
        // Channels are visited in order, so verbatim samples stay in channel order.
        sf.typ = SUB_VERBATIM;
        sf.order = 0;
        res.clear();
        f.verbatim.extend_from_slice(x);
    }
}

/// LPC residuals of `x` (from `coefs.len()` on); false if one does not fit 32 bits.
fn residual_lpc(x: &[i64], coefs: &[i32], shift: u32, res: &mut Vec<i32>) -> bool {
    let order = coefs.len();
    let mut ok = true;
    res.reserve(x.len() - order);
    for t in order..x.len() {
        let mut acc = 0i64;
        for (j, &c) in coefs.iter().enumerate() {
            acc = acc.wrapping_add(c as i64 * x[t - 1 - j]);
        }
        let e = x[t].wrapping_sub(acc >> shift);
        ok &= e as i32 as i64 == e;
        res.push(e as i32);
    }
    ok
}

/// Sum of absolute second differences (a cheap estimate of the coded size).
fn cost(x: &[i64]) -> u64 {
    x.windows(3).map(|w| (w[2] - 2 * w[1] + w[0]).unsigned_abs()).sum()
}

/// Builds the frame of one block of samples (`ch[c]`: channel c).
fn make_frame(f: &mut Frame, ch: &mut [Vec<i64>], fmt: &Format, win: &[f64]) {
    let n = ch[0].len();
    let bits = 8 * fmt.bytes as u32;
    f.block_size = n;
    f.channels = fmt.channels;
    f.bps = bits;
    f.sub.resize(fmt.channels, Default::default());
    f.res.resize(fmt.channels, Vec::new());
    f.verbatim.clear();
    f.assign = fmt.channels as u8 - 1;
    if fmt.channels == 2 && n >= 3 {
        let (l, r) = (&ch[0], &ch[1]);
        let side: Vec<i64> = l.iter().zip(r).map(|(a, b)| a - b).collect();
        let mid: Vec<i64> = l.iter().zip(r).map(|(a, b)| (a + b) >> 1).collect();
        let (cl, cr, cs, cm) = (cost(l), cost(r), cost(&side), cost(&mid));
        let opts = [(cl + cr, 1u8), (cl + cs, 8), (cs + cr, 9), (cm + cs, 10)];
        let best = opts.iter().min_by_key(|o| o.0).unwrap().1;
        f.assign = best;
        match best {
            8 => ch[1] = side,
            9 => ch[0] = side,
            10 => {
                ch[0] = mid;
                ch[1] = side;
            }
            _ => {}
        }
    }
    for c in 0..fmt.channels {
        let side = matches!((f.assign, c), (8, 1) | (9, 0) | (10, 1));
        subframe(f, c, &mut ch[c], bits + side as u32, win);
    }
}

/// Splits the samples of `d` into the channels.
#[inline(always)]
fn read_block(d: &[u8], fmt: &Format, ch: &mut [Vec<i64>]) {
    for c in ch.iter_mut() {
        c.clear();
    }
    for (i, smp) in d.chunks_exact(fmt.bytes).enumerate() {
        ch[i % fmt.channels].push(fmt.read(smp));
    }
}

/// Calls `per_frame` with the frame of each block of samples of `d` (whole sample frames).
fn frames(d: &[u8], fmt: Format, mut per_frame: impl FnMut(&mut Frame)) {
    let frame_bytes = fmt.frame_bytes();
    let data_end = d.len() / frame_bytes * frame_bytes;
    let mut f = Frame::default();
    let mut ch: Vec<Vec<i64>> = vec![Vec::with_capacity(BLOCK); fmt.channels];
    let full_win = window(BLOCK);
    let mut p = 0;
    while p < data_end {
        let n = BLOCK.min((data_end - p) / frame_bytes);
        read_block(&d[p..p + n * frame_bytes], &fmt, &mut ch);
        p += n * frame_bytes;
        let short;
        let win = if n == BLOCK {
            &full_win
        } else {
            short = window(n);
            &short
        };
        make_frame(&mut f, &mut ch, &fmt, win);
        per_frame(&mut f);
    }
}

/// Adds the (context, token) counts of the residuals of samples `d` to `counts` (for
/// training).
pub fn count(d: &[u8], fmt: Format, counts: &mut [Vec<u64>]) {
    if !fmt.valid() {
        return;
    }
    let mut rc = ResCounter::default();
    frames(d, fmt, |f| rc.frame(f, counts));
}

/// Encodes the samples `d`, whole sample frames of `fmt`.
pub fn encode(d: &[u8], fmt: Format) -> Option<Coded> {
    if !fmt.valid() || !d.len().is_multiple_of(fmt.frame_bytes()) {
        return None;
    }
    let mut s: Vec<Vec<u8>> = vec![Vec::new(); N_BYTE_STREAMS];
    let mut re = ResEncoder::new(d.len());
    frames(d, fmt, |f| {
        s[S_HDR].push(f.assign);
        let [sub, warm] = s.get_disjoint_mut([S_SUB, S_WARM]).unwrap();
        put_subframes(f, sub, warm, false);
        re.frame(f);
    });
    let (words, raw) = re.finish();
    Some(Coded { streams: s, words, raw })
}

/// [`encode`] with `threads` threads: a pool makes the frames of the blocks ahead (stereo
/// choice, LPC, residuals); one thread takes them in order through the LMS stage, and this
/// one codes them. The same output.
pub fn encode_threads(d: &[u8], fmt: Format, threads: usize) -> Option<Coded> {
    if threads <= 1 {
        return encode(d, fmt);
    }
    if !fmt.valid() || !d.len().is_multiple_of(fmt.frame_bytes()) {
        return None;
    }
    let frame_bytes = fmt.frame_bytes();
    let nframes = d.len() / frame_bytes;
    let mut s: Vec<Vec<u8>> = vec![Vec::new(); N_BYTE_STREAMS];
    let nblocks = nframes.div_ceil(BLOCK);
    let per = (par::BATCH_SAMPLES / (BLOCK * fmt.channels)).max(1);
    let nbatches = nblocks.div_ceil(per);
    // the LMS stage and a pool of one at least, on workers of the pool
    let workers = aqr_common::pool::reserve((threads - 1).max(2));
    if workers.len() < 2 {
        return encode(d, fmt);
    }
    let pool = workers.len() - 1;
    let nb = par::in_flight(threads);
    let full_win = window(BLOCK);
    aqr_common::pool::scope_in(workers, |sc| {
        let (jtx, jrx) = sync_channel::<EncBatch>(nb);
        let (ptx, prx) = sync_channel(nb);
        let (ltx, lrx) = sync_channel::<EncBatch>(nb);
        let mut ch: Vec<Vec<i64>> = vec![Vec::with_capacity(BLOCK); fmt.channels];
        let win = &full_win;
        par::pool(sc, pool, jrx, ptx, move |mut b: EncBatch| {
            b.n = 0;
            for k in b.idx * per..((b.idx + 1) * per).min(nblocks) {
                let p = k * BLOCK * frame_bytes;
                let n = BLOCK.min(nframes - k * BLOCK);
                read_block(&d[p..p + n * frame_bytes], &fmt, &mut ch);
                let short;
                let w = if n == BLOCK {
                    win
                } else {
                    short = window(n);
                    &short
                };
                make_frame(b.next_frame(), &mut ch, &fmt, w);
                b.n += 1;
            }
            b
        });
        let lms = sc.spawn(move || {
            let mut le = LmsEncoder::default();
            let mut ro = par::Reorder::default();
            for b in prx {
                let b: EncBatch = par::unwrap(b);
                ro.put(b.idx, b);
                while let Some(mut b) = ro.pop() {
                    for f in &mut b.frames[..b.n] {
                        s[S_HDR].push(f.assign);
                        let [sub, warm] = s.get_disjoint_mut([S_SUB, S_WARM]).unwrap();
                        put_subframes(f, sub, warm, false);
                        le.frame(f);
                    }
                    if ltx.send(b).is_err() {
                        return s;
                    }
                }
            }
            s
        });
        let mut re = ResEncoder::new(d.len());
        let mut next = 0;
        while next < nbatches.min(nb) {
            jtx.send(EncBatch { idx: next, ..Default::default() }).expect("pipeline");
            next += 1;
        }
        for _ in 0..nbatches {
            let mut b = lrx.recv().expect("pipeline");
            for f in &mut b.frames[..b.n] {
                re.code(f);
            }
            if next < nbatches {
                b.idx = next;
                jtx.send(b).expect("pipeline");
                next += 1;
            }
        }
        drop(jtx);
        let s = lms.join().unwrap_or_else(|p| std::panic::resume_unwind(p));
        let (words, raw) = re.finish();
        Some(Coded { streams: s, words, raw })
    })
}

/// Decodes the `len` bytes of samples of `fmt` into `out` (appending).
pub fn decode(c: &Coded, fmt: Format, t: &Tables, len: usize, out: &mut Vec<u8>) -> Result<(), Error> {
    if c.streams.len() != N_BYTE_STREAMS || !fmt.valid() {
        return Err(Error("stream count"));
    }
    let frame_bytes = fmt.frame_bytes();
    if !len.is_multiple_of(frame_bytes) {
        return Err(Error("length"));
    }
    let nframes = len / frame_bytes;
    out.reserve(len);
    let mut hdr = Rd::new(&c.streams[S_HDR]);
    let mut sub = Rd::new(&c.streams[S_SUB]);
    let mut warm = Rd::new(&c.streams[S_WARM]);
    let raw_buf = [bits::padded(&c.raw[0]), bits::padded(&c.raw[1])];
    let mut rd = ResDecoder::new(t, &c.words, &raw_buf)?;
    let mut f = Frame::default();
    let mut ch: Vec<Vec<i64>> = vec![Vec::with_capacity(BLOCK); fmt.channels];
    let mut done = 0;
    while done < nframes {
        let n = BLOCK.min(nframes - done);
        done += n;
        pcm_frame(&mut f, &fmt, n, &mut hdr, &mut sub, &mut warm)?;
        rd.frame(&mut f)?;
        let at = out.len();
        out.resize(at + n * frame_bytes, 0);
        synth(&f, &fmt, &mut ch, &mut out[at..]);
    }
    rd.finish()
}

/// The samples of a decoded frame, interleaved into `o`.
#[inline(always)]
fn synth(f: &Frame, fmt: &Format, ch: &mut [Vec<i64>], o: &mut [u8]) {
    for (c, v) in ch.iter_mut().enumerate() {
        frame::decode_channel(f, c, v);
    }
    if fmt.channels == 2 {
        let (a, b) = ch.split_at_mut(1);
        let (a, b) = (&mut a[0], &mut b[0]);
        match f.assign {
            8 => b.iter_mut().zip(a.iter()).for_each(|(s, &l)| *s = l.wrapping_sub(*s)),
            9 => a.iter_mut().zip(b.iter()).for_each(|(s, &r)| *s = s.wrapping_add(r)),
            10 => a.iter_mut().zip(b.iter_mut()).for_each(|(m, s)| {
                let mm = (*m << 1) | (*s & 1);
                *m = mm.wrapping_add(*s) >> 1;
                *s = mm.wrapping_sub(*s) >> 1;
            }),
            _ => {}
        }
    }
    for i in 0..f.block_size {
        for (c, v) in ch.iter().enumerate() {
            let p = (i * fmt.channels + c) * fmt.bytes;
            fmt.write(v[i], &mut o[p..p + fmt.bytes]);
        }
    }
}

/// The first stage of [`decode_threads`]: channel assignments and subframe parameters, and
/// where each frame's samples go (`place` given the batch's own data and the frame's bytes).
fn front<'a, X: Default>(
    e: &mut par::Front<'a, impl par::Pass<'a, X>, X>,
    c: &'a Coded,
    fmt: Format,
    nframes: usize,
    mut place: impl FnMut(&mut X, usize),
) -> Result<(), par::Stop<'a>> {
    let mut hdr = Rd::new(&c.streams[S_HDR]);
    let mut sub = Rd::new(&c.streams[S_SUB]);
    let mut warm = Rd::new(&c.streams[S_WARM]);
    let frame_bytes = fmt.channels * fmt.bytes;
    let mut done = 0;
    while done < nframes {
        let n = BLOCK.min(nframes - done);
        done += n;
        let mut f = e.frame();
        if let Err(err) = pcm_frame(&mut f, &fmt, n, &mut hdr, &mut sub, &mut warm) {
            e.unused(f);
            return Err(err.into());
        }
        place(e.x(), n * frame_bytes);
        e.push(Item::Frame(f), n * fmt.channels)?;
    }
    Ok(())
}

/// Reads a frame's channel assignment and subframe parameters into `f`.
#[inline(always)]
fn pcm_frame(f: &mut Frame, fmt: &Format, n: usize, hdr: &mut Rd, sub: &mut Rd, warm: &mut Rd) -> Result<(), Error> {
    f.block_size = n;
    f.channels = fmt.channels;
    f.bps = 8 * fmt.bytes as u32;
    f.assign = hdr.byte()?;
    let ok = if fmt.channels == 2 { [1, 8, 9, 10].contains(&f.assign) } else { f.assign as usize == fmt.channels - 1 };
    if !ok {
        return Err(Error("channel assignment"));
    }
    get_subframes(f, sub, warm, false)
}

/// [`decode`] with `threads` threads: a pipeline of the subframe parameters, the error
/// decoding and the LMS stage, then a pool reconstructs the samples into their place in
/// `out`. The same output and errors (the contents of `out` after an error differ).
pub fn decode_threads(
    c: &Coded,
    fmt: Format,
    t: &Tables,
    len: usize,
    out: &mut Vec<u8>,
    threads: usize,
) -> Result<(), Error> {
    if threads <= 1 {
        return decode(c, fmt, t, len, out);
    }
    if c.streams.len() != N_BYTE_STREAMS || !fmt.valid() {
        return Err(Error("stream count"));
    }
    let frame_bytes = fmt.frame_bytes();
    if !len.is_multiple_of(frame_bytes) {
        return Err(Error("length"));
    }
    #[cfg(target_family = "wasm")]
    return decode_apart(c, fmt, t, len, out, threads);
    // three stages and a pool of one at least, on workers of the pool
    #[allow(unreachable_code)]
    let workers = aqr_common::pool::reserve((threads - 1).max(4));
    if workers.len() < 4 {
        return decode(c, fmt, t, len, out);
    }
    let pool = workers.len() - 3;
    let nframes = len / frame_bytes;
    out.reserve(len);
    let raw_buf = [bits::padded(&c.raw[0]), bits::padded(&c.raw[1])];
    let rd = ResDecoder::new(t, &c.words, &raw_buf)?;
    let at = out.len();
    out.resize(at + nframes * frame_bytes, 0);
    let data = &mut out[at..];
    let nb = par::in_flight(threads);
    type B<'a> = DecBatch<'a, Vec<&'a mut [u8]>>;
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
            if let Some(mut e) = par::Front::new(par::Chans { recycle: &rrx, tx: &tx1 }, Vec::clear) {
                let mut rest = data;
                let place = |x: &mut Vec<_>, n| {
                    let (o, r) = std::mem::take(&mut rest).split_at_mut(n);
                    rest = r;
                    x.push(o);
                };
                let r = front(&mut e, c, fmt, nframes, place);
                e.finish(r);
            }
        });
        sc.spawn(move || par::errors_stage(rd, rx1, tx2));
        sc.spawn(move || par::lms_stage(rx2, tx3));
        let mut ch: Vec<Vec<i64>> = vec![Vec::with_capacity(BLOCK); fmt.channels];
        par::pool(sc, pool, rx3, otx, move |mut b: B| {
            let mut k = 0;
            for it in &b.items {
                if let Item::Frame(f) = it {
                    synth(f, &fmt, &mut ch, b.x[k]);
                    k += 1;
                }
            }
            b
        });
        // There is at most one error (the stages stop at it), so the order does not matter.
        let (mut seen, mut end) = (0, usize::MAX);
        while seen < end {
            let Ok(b) = orx.recv() else { return Err(Error("pipeline")) };
            let b = par::unwrap(b);
            seen += 1;
            for it in &b.items {
                if let Item::FrameErr(e) | Item::Err(e) = it {
                    return Err(Error(e.0));
                }
            }
            if b.last {
                end = b.idx + 1;
            }
            let _ = rtx.send(b);
        }
        Ok(())
    })
}

/// [`decode_threads`] in WebAssembly: the pipeline of tasks ([`par::apart`]), whose workers
/// reconstruct the samples of batches, which are appended here in order.
#[cfg(target_family = "wasm")]
fn decode_apart(
    c: &Coded,
    fmt: Format,
    t: &Tables,
    len: usize,
    out: &mut Vec<u8>,
    threads: usize,
) -> Result<(), Error> {
    let frame_bytes = fmt.frame_bytes();
    let nframes = len / frame_bytes;
    out.reserve(len);
    let raw_buf = [bits::padded(&c.raw[0]), bits::padded(&c.raw[1])];
    let rd = ResDecoder::new(t, &c.words, &raw_buf)?;
    let work = |b: &DecBatch<'_, ()>| {
        let mut ch: Vec<Vec<i64>> = vec![Vec::with_capacity(BLOCK); fmt.channels];
        let mut bytes = Vec::new();
        for it in &b.items {
            match it {
                Item::Frame(f) => {
                    let at = bytes.len();
                    bytes.resize(at + f.block_size * frame_bytes, 0);
                    synth(f, &fmt, &mut ch, &mut bytes[at..]);
                }
                Item::FrameErr(_) | Item::Err(_) => return (bytes, true),
                Item::Junk(_) => {}
            }
        }
        (bytes, false)
    };
    let take = |(bytes, failed): (Vec<u8>, bool), _| match failed {
        // (an error's message stays in its task)
        true => Err(Error("residuals")),
        false => {
            out.extend_from_slice(&bytes);
            Ok(())
        }
    };
    let front = |e: &mut _| front(e, c, fmt, nframes, |_: &mut (), _| {});
    par::apart::run(threads.saturating_sub(3), front, rd, work, take, Error("pipeline"))
}
