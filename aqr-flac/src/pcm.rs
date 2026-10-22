//! Compression of PCM audio (WAV, AIFF / AIFC, or headerless with a given format).
//!
//! The samples are cut into blocks, and each block becomes a FLAC-like frame (stereo
//! decorrelation, wasted bits, a quantised LPC predictor per channel) whose residuals go
//! through the same pipeline as FLAC residuals ([`crate::residual`]). Headers and
//! trailing bytes are kept verbatim.

use crate::bits;
use crate::flac::{self, Frame, SUB_CONSTANT, SUB_LPC, SUB_VERBATIM};
use crate::par::{self, DecBatch, EncBatch, Item};
use crate::rans::Tables;
use crate::residual::{
    Coded, Error, LmsEncoder, Rd, ResCounter, ResDecoder, ResEncoder, get_subframes, put_subframes, put_varint,
};
use std::sync::mpsc::sync_channel;

/// Byte streams (same count as FLAC's).
const S_PREFIX: usize = 0;
const S_LAYOUT: usize = 1;
const S_JUNK: usize = 2;
const S_HDR: usize = 3;
const S_SUB: usize = 4;
const S_WARM: usize = 5;
const N_BYTE_STREAMS: usize = 6;

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
    fn valid(&self) -> bool {
        (1..=flac::MAX_CHANNELS).contains(&self.channels) && (1..=4).contains(&self.bytes)
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

    fn to_bytes(self) -> [u8; 3] {
        [self.channels as u8, self.bytes as u8, self.big_endian as u8 | (self.unsigned as u8) << 1]
    }

    fn from_bytes(b: &[u8]) -> Option<Format> {
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

/// Finds the sample data of a WAV or AIFF file: (format, data offset, data length).
pub fn detect(d: &[u8]) -> Option<(Format, usize, usize)> {
    let u16le = |p: usize| u16::from_le_bytes(d[p..p + 2].try_into().unwrap()) as usize;
    let u32le = |p: usize| u32::from_le_bytes(d[p..p + 4].try_into().unwrap()) as usize;
    let u16be = |p: usize| u16::from_be_bytes(d[p..p + 2].try_into().unwrap()) as usize;
    let u32be = |p: usize| u32::from_be_bytes(d[p..p + 4].try_into().unwrap()) as usize;
    if d.len() >= 12 && &d[0..4] == b"RIFF" && &d[8..12] == b"WAVE" {
        let mut fmt = None;
        let mut p = 12;
        while p + 8 <= d.len() {
            let id = &d[p..p + 4];
            let len = u32le(p + 4);
            let body = p + 8;
            if id == b"fmt " && len >= 16 && body + 16 <= d.len() {
                let tag = u16le(body);
                let channels = u16le(body + 2);
                let align = u16le(body + 12);
                let bits = u16le(body + 14);
                let pcm = tag == 1 || (tag == 0xfffe && len >= 40 && body + 26 <= d.len() && u16le(body + 24) == 1);
                if !pcm || channels == 0 || align % channels != 0 || bits == 0 || bits > 8 * (align / channels) {
                    return None;
                }
                let bytes = align / channels;
                fmt = Some(Format { channels, bytes, big_endian: false, unsigned: bytes == 1 });
            } else if id == b"data" {
                let f = fmt?;
                // The size may be missing (streamed) or wrong: clamp to the file.
                let len = if len == 0 || len == 0xffff_ffff { d.len() - body } else { len.min(d.len() - body) };
                return f.valid().then_some((f, body, len));
            }
            p = body + len + (len & 1);
        }
    } else if d.len() >= 12 && &d[0..4] == b"FORM" && (&d[8..12] == b"AIFF" || &d[8..12] == b"AIFC") {
        let aifc = &d[8..12] == b"AIFC";
        let mut fmt = None;
        let mut p = 12;
        while p + 8 <= d.len() {
            let id = &d[p..p + 4];
            let len = u32be(p + 4);
            let body = p + 8;
            if id == b"COMM" && len >= 18 && body + 18 <= d.len() {
                let channels = u16be(body);
                let bits = u16be(body + 6);
                let little = if aifc {
                    if body + 22 > d.len() {
                        return None;
                    }
                    match &d[body + 18..body + 22] {
                        b"NONE" | b"twos" => false,
                        b"sowt" => true,
                        _ => return None,
                    }
                } else {
                    false
                };
                if bits == 0 || bits > 32 {
                    return None;
                }
                fmt = Some(Format { channels, bytes: bits.div_ceil(8), big_endian: !little, unsigned: false });
            } else if id == b"SSND" && body + 8 <= d.len() {
                let f = fmt?;
                let start = body + 8 + u32be(body);
                if start > d.len() {
                    return None;
                }
                let len = len.saturating_sub(8 + u32be(body)).min(d.len() - start);
                return f.valid().then_some((f, start, len));
            }
            p = body + len + (len & 1);
        }
    }
    None
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
        sf.typ = flac::SUB_FIXED + 1;
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

/// Calls `per_frame` with the frame of each block of samples of `d[start..start + len]`.
fn frames(d: &[u8], fmt: Format, start: usize, len: usize, mut per_frame: impl FnMut(&mut Frame)) {
    let frame_bytes = fmt.channels * fmt.bytes;
    let data_end = start + len / frame_bytes * frame_bytes;
    let mut f = Frame::default();
    let mut ch: Vec<Vec<i64>> = vec![Vec::with_capacity(BLOCK); fmt.channels];
    let full_win = window(BLOCK);
    let mut p = start;
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

/// Adds the (context, token) counts of a WAV / AIFF file's residuals to `counts` (for
/// training).
pub fn count(d: &[u8], counts: &mut [Vec<u64>]) {
    let Some((fmt, start, len)) = detect(d) else { return };
    let mut rc = ResCounter::default();
    frames(d, fmt, start, len, |f| rc.frame(f, counts));
}

/// Encodes the samples of `d[start..start + len]` (whole sample frames; the rest of `d`
/// is kept verbatim).
pub fn encode(d: &[u8], fmt: Format, start: usize, len: usize) -> Option<Coded> {
    if !fmt.valid() || start + len > d.len() {
        return None;
    }
    let frame_bytes = fmt.channels * fmt.bytes;
    let nframes = len / frame_bytes;
    let data_end = start + nframes * frame_bytes;
    let mut s: Vec<Vec<u8>> = vec![Vec::new(); N_BYTE_STREAMS];
    s[S_PREFIX].extend_from_slice(&d[..start]);
    s[S_LAYOUT].extend_from_slice(&fmt.to_bytes());
    put_varint(&mut s[S_LAYOUT], nframes as u64);
    s[S_JUNK].extend_from_slice(&d[data_end..]);
    let mut re = ResEncoder::new(len);
    frames(d, fmt, start, len, |f| {
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
pub fn encode_threads(d: &[u8], fmt: Format, start: usize, len: usize, threads: usize) -> Option<Coded> {
    if threads <= 1 {
        return encode(d, fmt, start, len);
    }
    if !fmt.valid() || start + len > d.len() {
        return None;
    }
    let frame_bytes = fmt.channels * fmt.bytes;
    let nframes = len / frame_bytes;
    let data_end = start + nframes * frame_bytes;
    let mut s: Vec<Vec<u8>> = vec![Vec::new(); N_BYTE_STREAMS];
    s[S_PREFIX].extend_from_slice(&d[..start]);
    s[S_LAYOUT].extend_from_slice(&fmt.to_bytes());
    put_varint(&mut s[S_LAYOUT], nframes as u64);
    s[S_JUNK].extend_from_slice(&d[data_end..]);
    let nblocks = nframes.div_ceil(BLOCK);
    let per = (par::BATCH_SAMPLES / (BLOCK * fmt.channels)).max(1);
    let nbatches = nblocks.div_ceil(per);
    let nb = par::in_flight(threads);
    let full_win = window(BLOCK);
    std::thread::scope(|sc| {
        let (jtx, jrx) = sync_channel::<EncBatch>(nb);
        let (ptx, prx) = sync_channel(nb);
        let (ltx, lrx) = sync_channel::<EncBatch>(nb);
        let mut ch: Vec<Vec<i64>> = vec![Vec::with_capacity(BLOCK); fmt.channels];
        let win = &full_win;
        par::pool(sc, threads.saturating_sub(2), jrx, ptx, move |mut b: EncBatch| {
            b.n = 0;
            for k in b.idx * per..((b.idx + 1) * per).min(nblocks) {
                let p = start + k * BLOCK * frame_bytes;
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
        let mut re = ResEncoder::new(len);
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

/// Decodes into `out` (appending).
pub fn decode(c: &Coded, t: &Tables, total_len: usize, out: &mut Vec<u8>) -> Result<(), Error> {
    if c.streams.len() != N_BYTE_STREAMS {
        return Err(Error("stream count"));
    }
    let mut layout = Rd::new(&c.streams[S_LAYOUT]);
    let fmt = Format::from_bytes(layout.bytes(3)?).ok_or(Error("format"))?;
    let nframes = layout.varint()? as usize;
    let frame_bytes = fmt.channels * fmt.bytes;
    let prefix = &c.streams[S_PREFIX];
    let junk = &c.streams[S_JUNK];
    if nframes.checked_mul(frame_bytes).and_then(|v| v.checked_add(prefix.len() + junk.len())) != Some(total_len) {
        return Err(Error("length"));
    }
    out.reserve(total_len);
    out.extend_from_slice(prefix);
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
    out.extend_from_slice(junk);
    rd.finish()
}

/// The samples of a decoded frame, interleaved into `o`.
#[inline(always)]
fn synth(f: &Frame, fmt: &Format, ch: &mut [Vec<i64>], o: &mut [u8]) {
    for (c, v) in ch.iter_mut().enumerate() {
        flac::decode_channel(f, c, v);
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

/// The first stage of [`decode_threads`]: channel assignments and subframe parameters,
/// and where each frame's samples go.
fn front<'a>(
    e: &mut par::Front<'a, '_, Vec<&'a mut [u8]>>,
    c: &'a Coded,
    fmt: Format,
    nframes: usize,
    mut rest: &'a mut [u8],
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
        let (o, r) = std::mem::take(&mut rest).split_at_mut(n * frame_bytes);
        rest = r;
        e.x().push(o);
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
pub fn decode_threads(c: &Coded, t: &Tables, total_len: usize, out: &mut Vec<u8>, threads: usize) -> Result<(), Error> {
    if threads <= 1 {
        return decode(c, t, total_len, out);
    }
    if c.streams.len() != N_BYTE_STREAMS {
        return Err(Error("stream count"));
    }
    let mut layout = Rd::new(&c.streams[S_LAYOUT]);
    let fmt = Format::from_bytes(layout.bytes(3)?).ok_or(Error("format"))?;
    let nframes = layout.varint()? as usize;
    let frame_bytes = fmt.channels * fmt.bytes;
    let prefix = &c.streams[S_PREFIX];
    let junk = &c.streams[S_JUNK];
    if nframes.checked_mul(frame_bytes).and_then(|v| v.checked_add(prefix.len() + junk.len())) != Some(total_len) {
        return Err(Error("length"));
    }
    out.reserve(total_len);
    out.extend_from_slice(prefix);
    let raw_buf = [bits::padded(&c.raw[0]), bits::padded(&c.raw[1])];
    let rd = ResDecoder::new(t, &c.words, &raw_buf)?;
    let at = out.len();
    out.resize(at + nframes * frame_bytes, 0);
    let data = &mut out[at..];
    let nb = par::in_flight(threads);
    type B<'a> = DecBatch<'a, Vec<&'a mut [u8]>>;
    std::thread::scope(|sc| {
        let (rtx, rrx) = sync_channel::<B>(nb);
        let (tx1, rx1) = sync_channel::<B>(nb);
        let (tx2, rx2) = sync_channel::<B>(nb);
        let (tx3, rx3) = sync_channel::<B>(nb);
        let (otx, orx) = sync_channel::<Result<B, par::Panic>>(nb);
        for _ in 0..nb {
            rtx.send(B::default()).expect("channel");
        }
        sc.spawn(move || {
            if let Some(mut e) = par::Front::new(&rrx, &tx1, Vec::clear) {
                let r = front(&mut e, c, fmt, nframes, data);
                e.finish(r);
            }
        });
        sc.spawn(move || par::errors_stage(rd, rx1, tx2));
        sc.spawn(move || par::lms_stage(rx2, tx3));
        let mut ch: Vec<Vec<i64>> = vec![Vec::with_capacity(BLOCK); fmt.channels];
        par::pool(sc, threads.saturating_sub(2), rx3, otx, move |mut b: B| {
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
    })?;
    out.extend_from_slice(junk);
    Ok(())
}
