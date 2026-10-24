//! The token representation of a FlateDecode stream: its DEFLATE tokens (literals and
//! matches, as the encoder chose them) coded with static per-file tables chosen by context,
//! so that rebuilding the stream needs no match search, only Huffman coding.
//!
//! - Literals and length codes share one alphabet ([`LIT_ALPHA`]); the table is chosen by
//!   the byte before (per class, at the resolution that costs least for the file: every
//!   byte, 32 buckets, 8 buckets or none). Stored blocks' bytes are coded as literals.
//! - Distance codes have a table per length code (capped), and extra bits are raw.
//! - The block layout (types, token counts, stored lengths, padding) and the matches of
//!   length 258 sent as 284 + 31 go to a layout stream; dynamic Huffman headers go through
//!   aqr-flate's header coder, against the block's frequencies.
//!
//! All streams of a file share the layout, header and raw bit streams and one rANS stream,
//! coded and decoded in file order. They are the [`PARTS`] parts of the coded form: layout,
//! tables, rANS, raw bits, headers (each needs only the ones before it to be read).

use crate::kind::{CLASSES, Class};
use crate::rans::{self, BitR, BitW, Decoder, Tables};
use crate::{Error, Result};
use aqr_flate::bits::{BitReader, BitWriter};
use aqr_flate::deflate::{Block, Body, DynHeader, Stream, Token};
use aqr_flate::header::{self, Recipe};
use aqr_flate::huffman::{DIST_BASE, DIST_EXTRA, LEN_BASE, LEN_EXTRA};
use aqr_flate::inflate::Flat;
use aqr_raw::image::pixel::{ACT, bucket, med as med3};
use std::cell::Cell;

/// Literals 0-255, then length codes 0-28 (symbols 257-285).
pub const LIT_ALPHA: usize = 256 + 29;
pub const DIST_ALPHA: usize = 30;
const STRIDE: usize = 288;
/// Distance tables per class (by length code, capped).
const DIST_CTX: usize = 8;
/// Literal contexts at the finest resolution: the byte before.
const LIT_CTX: usize = 256;
/// Tables per class at the finest resolution.
const PER_CLASS: usize = LIT_CTX + DIST_CTX;

/// The length code of every length 3-258.
static LEN_CODE: [u8; 259] = {
    let mut t = [0u8; 259];
    let mut c = 0;
    while c < 29 {
        let lo = LEN_BASE[c] as usize;
        let hi = if c == 28 { 259 } else { LEN_BASE[c + 1] as usize };
        let mut l = lo;
        while l < hi && l < 259 {
            t[l] = c as u8;
            l += 1;
        }
        c += 1;
    }
    t[258] = 28;
    t
};

#[inline(always)]
fn dist_code(d: u32) -> usize {
    // 32 - lz(d - 1) is the bit length; codes come in pairs per bit length.
    if d <= 4 {
        return d as usize - 1;
    }
    let x = d - 1;
    let nb = 31 - x.leading_zeros();
    (2 * nb + ((x >> (nb - 1)) & 1)) as usize
}

/// Coarse byte classes for text-like data (content streams, object streams): buckets of
/// similar bytes, grouped by fours so that `bucket >> 2` is a coarser class.
static TEXT_MAP: [u8; 256] = {
    let mut t = [27u8; 256];
    let mut b = 0;
    while b < 256 {
        let c = b as u8;
        t[b] = match c {
            b'0' | b'1' => 0,
            b'2'..=b'4' => 1,
            b'5'..=b'7' => 2,
            b'8' | b'9' => 3,
            b' ' => 4,
            b'\n' => 5,
            b'\r' => 6,
            b'\t' | 0x0c | 0 => 7,
            b'.' => 8,
            b'-' => 9,
            b'+' => 10,
            b',' => 11,
            b'a'..=b'f' => 12,
            b'g'..=b'm' => 13,
            b'n'..=b's' => 14,
            b't'..=b'z' => 15,
            b'A'..=b'F' => 16,
            b'G'..=b'M' => 17,
            b'N'..=b'S' => 18,
            b'T'..=b'Z' => 19,
            b'/' => 20,
            b'(' | b')' => 21,
            b'<' | b'>' => 22,
            b'[' | b']' | b'{' | b'}' => 23,
            b'\\' => 24,
            b'%' | b'#' => 25,
            b'*' | b'\'' | b'"' | b'_' => 26,
            0x80..=0x9f => 28,
            0xa0..=0xbf => 29,
            0xc0..=0xdf => 30,
            0xe0..=0xff => 31,
            _ => 27,
        };
        b += 1;
    }
    t
};

static BIN_MAP: [u8; 256] = {
    let mut t = [0u8; 256];
    let mut b = 0;
    while b < 256 {
        t[b] = (b >> 3) as u8;
        b += 1;
    }
    t
};

fn bucket_map(c: Class) -> &'static [u8; 256] {
    match c {
        Class::Font | Class::Image => &BIN_MAP,
        _ => &TEXT_MAP,
    }
}

/// Literal context resolutions: every byte, 32 buckets, 8 buckets, one (pixels: channel
/// and activity, activity, channel, one).
const RESOLUTIONS: usize = 4;

/// Table sets: one per class, and one for images coded as pixels.
const PIXEL: usize = CLASSES.len();
const SLOTS: usize = PIXEL + 1;
/// Pixel contexts: whether a match came just before × channel (capped at 3) × activity.
const PIX_CTX: usize = 8 * ACT;

/// How an image's inflated data is laid out, for the pixel model: rows of `stride` bytes of
/// `bpp`-byte pixels, no filter bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pixels {
    pub bpp: usize,
    pub stride: usize,
}

/// MED's prediction for the byte at `p` (column `col`) from left `a`, up `b` and up-left
/// `c` (zero outside the image), and the activity `|a - c| + |b - c|`.
#[inline(always)]
fn med(d: &[u8], p: usize, col: usize, px: Pixels) -> (i32, u32) {
    let left = col >= px.bpp;
    let up = p >= px.stride;
    let a = if left { d[p - px.bpp] } else { 0 } as i32;
    let b = if up { d[p - px.stride] } else { 0 } as i32;
    let c = if left && up { d[p - px.stride - px.bpp] } else { 0 } as i32;
    (med3(a, b, c), ((a - c).abs() + (b - c).abs()) as u32)
}

/// The pixel model's prediction and context for the byte at `p`: MED, plus for every
/// channel but the first the previous channel's residual (inter-channel decorrelation); the
/// context is the channel (capped) and the activity. `last`: the position and MED of the
/// last call, reused when it was the byte before.
#[inline(always)]
fn pixel_ctx(
    d: &[u8],
    p: usize,
    col: usize,
    ch: usize,
    after: bool,
    px: Pixels,
    last: &Cell<(usize, i32)>,
) -> (u8, usize) {
    let (m, act) = med(d, p, col, px);
    let mut pred = m;
    if ch > 0 {
        let (at, v) = last.get();
        let prev = if at == p - 1 { v } else { med(d, p - 1, col - 1, px).0 };
        pred += d[p - 1] as i32 - prev;
    }
    last.set((p, m));
    (pred as u8, ((after as usize) * 4 + ch.min(3)) * ACT + bucket(act))
}

/// The literal table of fine context `t` of slot `slot` at resolution `res`.
#[inline(always)]
fn coarse(slot: usize, res: u8, t: usize) -> usize {
    if slot == PIXEL {
        match res {
            0 => t,
            1 => t % ACT,
            2 => t / ACT,
            _ => 0,
        }
    } else {
        lit_ctx(bucket_map(CLASSES[slot]), res, t as u8)
    }
}

fn slot_tables(slot: usize, res: u8) -> usize {
    if slot == PIXEL { [PIX_CTX, ACT, 8, 1][res as usize] } else { res_tables(res) }
}

/// The literal table (0..) of byte `b` at resolution `res`.
#[inline(always)]
fn lit_ctx(map: &[u8; 256], res: u8, b: u8) -> usize {
    match res {
        0 => b as usize,
        1 => map[b as usize] as usize,
        2 => (map[b as usize] >> 2) as usize,
        _ => 0,
    }
}

fn res_tables(res: u8) -> usize {
    [256, 32, 8, 1][res as usize]
}

/// Copies a match of `len` bytes from `dist` back to `o`, in 8-byte chunks (the buffer has
/// room for the overshoot). Distances under 8 copy their first period byte by byte, then
/// chunks from a multiple of the distance.
#[inline(always)]
fn copy_match(d: &mut [u8], o: usize, dist: usize, len: usize) {
    assert!(dist >= 1 && dist <= o && o + len + 8 <= d.len());
    let p = d.as_mut_ptr();
    // SAFETY: by the assertion, every chunk reads from `o - dist` on and writes below
    // `o + len + 8`, within the buffer; chunks read only bytes already written.
    unsafe {
        let chunk = |from: usize, to: usize| {
            let v = std::ptr::read_unaligned(p.add(from) as *const u64);
            std::ptr::write_unaligned(p.add(to) as *mut u64, v);
        };
        if dist >= 8 {
            let mut k = 0;
            while k < len {
                chunk(o + k - dist, o + k);
                k += 8;
            }
        } else if dist == 1 {
            std::ptr::write_bytes(p.add(o), *p.add(o - 1), len);
        } else {
            let m = [0, 0, 8, 9, 8, 10, 12, 14][dist];
            let first = m.min(len);
            for k in 0..first {
                *p.add(o + k) = *p.add(o + k - dist);
            }
            let mut k = first;
            while k < len {
                chunk(o + k - m, o + k);
                k += 8;
            }
        }
    }
}

/// Room after the data for overlapping copies.
const SLACK: usize = 16;

/// The parts of the coded form.
pub const PARTS: usize = 5;

/// Whether pixels of `bpp` bytes tile rows of `stride` bytes.
fn tiles(px: Pixels) -> bool {
    px.bpp > 0 && px.stride >= px.bpp && px.stride.is_multiple_of(px.bpp)
}

/// The encoder of a file's token streams.
pub struct Encoder {
    syms: Vec<(u16, u16)>,
    raw: BitW,
    layout: BitW,
    headers: BitWriter,
    recipe: Recipe,
    /// Literal and distance counts per slot at the finest resolution (allocated on use).
    counts: Vec<Vec<u32>>,
    /// Raw bits per slot.
    raw_bits: [u64; SLOTS],
}

impl Default for Encoder {
    fn default() -> Self {
        Encoder {
            syms: vec![],
            raw: BitW::default(),
            layout: BitW::default(),
            headers: BitWriter::new(),
            recipe: Recipe::default(),
            counts: vec![vec![]; SLOTS],
            raw_bits: [0; SLOTS],
        }
    }
}

impl Encoder {
    /// Adds a parsed stream (with no trailing bytes) of class `c`, an image coded as pixels
    /// if `px` is given.
    pub fn add(&mut self, c: Class, f: &Flat, px: Option<Pixels>) -> Result<()> {
        if !f.stream.trailing.is_empty() || px.is_some_and(|p| !tiles(p)) {
            return Err(Error::Corrupt);
        }
        let ci = if px.is_some() { PIXEL } else { c as usize };
        if self.counts[ci].is_empty() {
            self.counts[ci] = vec![0; PER_CLASS * STRIDE];
        }
        let counts = &mut self.counts[ci];
        let base = (ci * PER_CLASS) as u16;
        // Layout.
        let w = &mut self.layout;
        let blocks = &f.stream.blocks;
        w.gamma(blocks.len() as u32);
        let std_last = blocks.iter().enumerate().all(|(i, b)| b.last == (i + 1 == blocks.len()));
        w.bits(1, std_last as u32);
        let mut prev_count = 0usize;
        for (i, b) in blocks.iter().enumerate() {
            if !std_last {
                w.bits(1, b.last as u32);
            }
            w.bits(2, b.body.btype() as u32);
            match b.body {
                Body::Stored { pad, len } => {
                    w.bits(8, pad as u32);
                    w.bits(16, len as u32);
                }
                _ => {
                    let count = f.starts[i + 1] - f.starts[i];
                    if count == prev_count {
                        w.bits(1, 1);
                    } else {
                        w.bits(1, 0);
                        w.gamma(count as u32);
                    }
                    prev_count = count;
                }
            }
        }
        w.bits(8, f.stream.tail_pad as u32);
        let mut last = 0usize;
        let alts: Vec<usize> = f
            .tokens
            .iter()
            .enumerate()
            .filter(|(_, t)| matches!(t, Token::Match { alt258: true, .. }))
            .map(|(i, _)| i)
            .collect();
        w.gamma(alts.len() as u32);
        for &i in &alts {
            w.gamma((i - last) as u32);
            last = i;
        }
        // Headers.
        for (b, (lf, df)) in blocks.iter().zip(&f.stats.freqs) {
            if let Body::Dynamic { header: h, .. } = &b.body {
                header::write(&mut self.headers, lf, df, h, &mut self.recipe);
            }
        }
        // Tokens, with stored blocks' bytes as literals.
        let d = &f.data;
        let mut p = 0usize;
        let mut skips = f.skips.iter().peekable();
        self.syms.reserve(f.tokens.len() + 2 * f.skips.len());
        // The fine literal context at `p`, and the byte the literal there is coded against.
        // `after`: a match came just before (pixels only).
        let last = Cell::new((usize::MAX, 0));
        let ctx = |p: usize, after: bool| -> (usize, u8) {
            match px {
                None => (if p > 0 { d[p - 1] } else { 0 } as usize, 0),
                Some(px) => {
                    let col = p % px.stride;
                    let (pred, t) = pixel_ctx(d, p, col, col % px.bpp, after, px, &last);
                    (t, pred)
                }
            }
        };
        let mut after = false;
        let lit = |p: usize, after: &mut bool, counts: &mut Vec<u32>, syms: &mut Vec<(u16, u16)>| {
            let (t, pred) = ctx(p, *after);
            *after = false;
            let v = d[p].wrapping_sub(pred);
            counts[t * STRIDE + v as usize] += 1;
            syms.push((base + t as u16, v as u16));
        };
        for (k, t) in f.tokens.iter().enumerate() {
            while let Some(&&(at, len)) = skips.peek()
                && at == k
            {
                for q in p..p + len {
                    lit(q, &mut after, counts, &mut self.syms);
                }
                p += len;
                skips.next();
            }
            match *t {
                Token::Lit(_) => {
                    lit(p, &mut after, counts, &mut self.syms);
                    p += 1;
                }
                Token::Match { len, dist, .. } => {
                    let prev = ctx(p, after).0;
                    after = true;
                    let lc = LEN_CODE[len as usize] as usize;
                    counts[prev * STRIDE + 256 + lc] += 1;
                    self.syms.push((base + prev as u16, (256 + lc) as u16));
                    self.raw.bits(LEN_EXTRA[lc] as u32, (len - LEN_BASE[lc]) as u32);
                    self.raw_bits[ci] += (LEN_EXTRA[lc] + DIST_EXTRA[dist_code(dist as u32)]) as u64;
                    let dc = dist_code(dist as u32);
                    let dt = LIT_CTX + lc.min(DIST_CTX - 1);
                    counts[dt * STRIDE + dc] += 1;
                    self.syms.push((base + dt as u16, dc as u16));
                    self.raw.bits(DIST_EXTRA[dc] as u32, (dist - DIST_BASE[dc]) as u32);
                    p += len as usize;
                }
            }
        }
        for &(_, len) in skips {
            for q in p..p + len {
                lit(q, &mut after, counts, &mut self.syms);
            }
            p += len;
        }
        debug_assert_eq!(p, d.len());
        Ok(())
    }

    /// The parts of the coded form (all empty if no stream was added); and the estimated
    /// bytes of each class, then of the pixel streams (tables, symbols and raw bits).
    pub fn finish(self) -> ([Vec<u8>; PARTS], [f64; CLASSES.len() + 1]) {
        let mut sizes = [0f64; SLOTS];
        if self.counts.iter().all(Vec::is_empty) {
            return (Default::default(), sizes);
        }
        let mut tw = BitW::default();
        // Per class: unused, or the literal resolution, then the tables in use.
        // Fine table -> table actually coded.
        let mut remap = vec![u16::MAX; SLOTS * PER_CLASS];
        let mut freqs: Vec<Vec<u32>> = vec![];
        for (ci, counts) in self.counts.iter().enumerate() {
            if counts.is_empty() {
                tw.bits(1, 0);
                continue;
            }
            tw.bits(1, 1);
            let nfine = if ci == PIXEL { PIX_CTX } else { LIT_CTX };
            // The cheapest resolution, data plus (estimated) tables, from the fine contexts'
            // nonzero counts (few, for small files).
            let mut rows: Vec<(usize, Vec<(u16, u32)>)> = vec![];
            for b in 0..nfine {
                let row = &counts[b * STRIDE..b * STRIDE + LIT_ALPHA];
                if row.iter().fold(0, |a, &x| a | x) == 0 {
                    continue;
                }
                let nz: Vec<(u16, u32)> =
                    row.iter().enumerate().filter(|&(_, &x)| x > 0).map(|(s, &x)| (s as u16, x)).collect();
                rows.push((b, nz));
            }
            let est_row = |nz: &mut dyn Iterator<Item = u32>| -> f64 {
                let (mut tot, mut acc, mut n) = (0f64, 0f64, 0usize);
                for x in nz {
                    let x = x as f64;
                    tot += x;
                    acc += x * x.log2();
                    n += 1;
                }
                tot * tot.log2() - acc + 6.0 * n as f64 + 16.0
            };
            let mut best: (f64, u8) = (rows.iter().map(|(_, nz)| est_row(&mut nz.iter().map(|e| e.1))).sum(), 0);
            for res in 1..RESOLUTIONS as u8 {
                let mut v = vec![0u32; slot_tables(ci, res) * LIT_ALPHA];
                for (b, nz) in &rows {
                    let t = coarse(ci, res, *b) * LIT_ALPHA;
                    for &(s, x) in nz {
                        v[t + s as usize] += x;
                    }
                }
                let e: f64 = v
                    .chunks_exact(LIT_ALPHA)
                    .map(|c| c.iter().copied().filter(|&x| x > 0))
                    .map(|mut c| match c.next() {
                        None => 0.0,
                        Some(x) => est_row(&mut std::iter::once(x).chain(c)),
                    })
                    .sum();
                if e < best.0 {
                    best = (e, res);
                }
            }
            let (bits, res) = best;
            let mut lits = vec![vec![0u32; LIT_ALPHA]; slot_tables(ci, res)];
            for (b, nz) in &rows {
                let t = &mut lits[coarse(ci, res, *b)];
                for &(s, x) in nz {
                    t[s as usize] += x;
                }
            }
            sizes[ci] = (bits + self.raw_bits[ci] as f64) / 8.0;
            for k in 0..DIST_CTX {
                let t = LIT_CTX + k;
                sizes[ci] += rans::cost(&counts[t * STRIDE..t * STRIDE + DIST_ALPHA]) / 8.0;
            }
            tw.bits(2, res as u32);
            let mut code = |c: &[u32], tw: &mut BitW| -> Option<u16> {
                let used = c.iter().any(|&x| x > 0);
                tw.bits(1, used as u32);
                used.then(|| {
                    freqs.push(rans::write_table(tw, c));
                    (freqs.len() - 1) as u16
                })
            };
            let lit_ids: Vec<Option<u16>> = lits.iter().map(|c| code(c, &mut tw)).collect();
            for b in 0..nfine {
                if let Some(id) = lit_ids[coarse(ci, res, b)] {
                    remap[ci * PER_CLASS + b] = id;
                }
            }
            for k in 0..DIST_CTX {
                let t = LIT_CTX + k;
                if let Some(id) = code(&counts[t * STRIDE..t * STRIDE + DIST_ALPHA], &mut tw) {
                    remap[ci * PER_CLASS + t] = id;
                }
            }
        }
        let syms: Vec<(u16, u16)> = self.syms.iter().map(|&(t, s)| (remap[t as usize], s)).collect();
        let st = rans::encode(&syms, &freqs);
        let tb = tw.finish();
        let raw = self.raw.finish();
        let layout = self.layout.finish();
        let mut hw = self.headers;
        let pad = hw.pad_len();
        hw.bits(pad, 0);
        let headers = hw.finish();
        ([layout, tb, st, raw, headers], sizes)
    }
}

/// How literals are coded: against what, in which table, and how the position moves.
trait LitModel {
    /// The byte the literal at `o` is coded against, and its fine context.
    fn ctx(&self, d: &[u8], o: usize) -> (u8, usize);
    fn lit(&mut self, b: u8);
    /// After a match of `len` bytes, ending at `o`.
    fn skip(&mut self, d: &[u8], o: usize, len: usize);
}

/// Text and binary data: the context is the byte before.
struct Prev(u8);

impl LitModel for Prev {
    #[inline(always)]
    fn ctx(&self, _: &[u8], _: usize) -> (u8, usize) {
        (0, self.0 as usize)
    }
    #[inline(always)]
    fn lit(&mut self, b: u8) {
        self.0 = b;
    }
    #[inline(always)]
    fn skip(&mut self, d: &[u8], o: usize, _: usize) {
        self.0 = d[o - 1];
    }
}

/// Pixels: the residual against MED, by channel and activity.
struct Pix {
    px: Pixels,
    col: usize,
    ch: usize,
    after: bool,
    last: Cell<(usize, i32)>,
}

impl LitModel for Pix {
    #[inline(always)]
    fn ctx(&self, d: &[u8], o: usize) -> (u8, usize) {
        pixel_ctx(d, o, self.col, self.ch, self.after, self.px, &self.last)
    }
    #[inline(always)]
    fn lit(&mut self, _: u8) {
        self.after = false;
        self.col += 1;
        if self.col == self.px.stride {
            self.col = 0;
        }
        self.ch += 1;
        if self.ch == self.px.bpp {
            self.ch = 0;
        }
    }
    #[inline(always)]
    fn skip(&mut self, _: &[u8], _: usize, len: usize) {
        self.after = true;
        self.col = (self.col + len) % self.px.stride;
        self.ch = (self.ch + len) % self.px.bpp;
    }
}

/// One stream's decoding state.
struct Cursor<'s, 'a> {
    tb: &'s Tables,
    cx: &'s [u16; PER_CLASS],
    dec: &'s mut Decoder<'a>,
    raw: &'s mut BitR<'a>,
    data: &'s mut Vec<u8>,
    tokens: &'s mut Vec<Token>,
    freqs: &'s mut Vec<([u32; 288], [u32; 32])>,
    o: usize,
    plain: usize,
}

impl Cursor<'_, '_> {
    /// Decodes the blocks (stored length, or token count) into data and tokens, with the
    /// blocks' symbol frequencies. `tokens` has room for all of them.
    #[inline(always)]
    fn blocks<M: LitModel>(&mut self, spans: &[(bool, usize)], m: &mut M) -> Result<()> {
        let (tb, cx, plain) = (self.tb, self.cx, self.plain);
        let mut dec = *self.dec;
        let mut o = self.o;
        let data = &mut self.data[..];
        for &(stored, n) in spans {
            if stored {
                if o + n > plain {
                    return Err(Error::Corrupt);
                }
                for _ in 0..n {
                    let (pred, t) = m.ctx(data, o);
                    let s = dec.get(tb, cx[t] as usize);
                    if s >= 256 {
                        return Err(Error::Corrupt);
                    }
                    let b = (s as u8).wrapping_add(pred);
                    data[o] = b;
                    m.lit(b);
                    o += 1;
                }
                self.freqs.push(([0u32; 288], [0u32; 32]));
                continue;
            }
            let mut lf = [0u32; 288];
            let mut df = [0u32; 32];
            lf[256] = 1;
            // The tokens go straight into the buffer (counted against its capacity).
            let tp = self.tokens.as_mut_ptr();
            let mut ti = self.tokens.len();
            if ti + n > self.tokens.capacity() {
                return Err(Error::Corrupt);
            }
            for _ in 0..n {
                let (pred, t) = m.ctx(data, o);
                let s = dec.get(tb, cx[t] as usize) as usize;
                let tok = if s < 256 {
                    if o >= plain {
                        return Err(Error::Corrupt);
                    }
                    let b = (s as u8).wrapping_add(pred);
                    data[o] = b;
                    o += 1;
                    lf[b as usize] += 1;
                    m.lit(b);
                    Token::Lit(b)
                } else {
                    let lc = s - 256;
                    let len = LEN_BASE[lc] as usize + self.raw.bits(LEN_EXTRA[lc] as u32) as usize;
                    let dc = dec.get(tb, cx[LIT_CTX + lc.min(DIST_CTX - 1)] as usize) as usize;
                    // 258 is code 28's alone (code 27 with extra bits 31 is not a length).
                    if dc >= DIST_ALPHA || len > 257 + (lc == 28) as usize {
                        return Err(Error::Corrupt);
                    }
                    let dist = DIST_BASE[dc] as usize + self.raw.bits(DIST_EXTRA[dc] as u32) as usize;
                    if dist > o || o + len > plain {
                        return Err(Error::Corrupt);
                    }
                    copy_match(data, o, dist, len);
                    o += len;
                    m.skip(data, o, len);
                    lf[257 + lc] += 1;
                    df[dc] += 1;
                    Token::Match { len: len as u16, dist: dist as u16, alt258: false }
                };
                // SAFETY: `ti` stays below the capacity (checked before the loop).
                unsafe { tp.add(ti).write(tok) };
                ti += 1;
            }
            // SAFETY: the first `ti` tokens were written.
            unsafe { self.tokens.set_len(ti) };
            self.freqs.push((lf, df));
        }
        *self.dec = dec;
        self.o = o;
        Ok(())
    }
}

/// The decoder of a file's token streams, in the order they were added.
pub struct DecoderState<'a> {
    tables: Tables,
    /// Per slot: the literal table of each fine context, then the distance tables.
    ctx: Vec<[u16; PER_CLASS]>,
    dec: Decoder<'a>,
    raw: BitR<'a>,
    layout: BitR<'a>,
    /// Bits of the layout: a block takes 3 at least.
    layout_bits: usize,
    headers: BitReader<'a>,
    recipe: Recipe,
}

impl<'a> DecoderState<'a> {
    /// A decoder of the parts given by [`Encoder::finish`] (not all empty).
    pub fn new(parts: [&'a [u8]; PARTS]) -> Result<DecoderState<'a>> {
        let [layout, tb, st, raw, headers] = parts;
        let mut tr = BitR::new(tb);
        let mut freqs: Vec<Vec<u32>> = vec![];
        let mut ctx = vec![[0u16; PER_CLASS]; SLOTS];
        let mut read = |tr: &mut BitR, n: usize| -> Result<Option<u16>> {
            if tr.bits(1) == 0 {
                return Ok(None);
            }
            freqs.push(rans::read_table(tr, n)?);
            Ok(Some((freqs.len() - 1) as u16))
        };
        // Unused contexts point at a table of their own (decoding them is an error later).
        let mut unused = vec![];
        for (ci, cx) in ctx.iter_mut().enumerate() {
            if tr.bits(1) == 0 {
                unused.push(ci);
                continue;
            }
            let res = tr.bits(2) as u8;
            let ids: Vec<Option<u16>> =
                (0..slot_tables(ci, res)).map(|_| read(&mut tr, LIT_ALPHA)).collect::<Result<_>>()?;
            let nfine = if ci == PIXEL { PIX_CTX } else { LIT_CTX };
            for (b, t) in cx.iter_mut().enumerate().take(nfine) {
                *t = ids[coarse(ci, res, b)].unwrap_or(u16::MAX);
            }
            for k in 0..DIST_CTX {
                cx[LIT_CTX + k] = read(&mut tr, DIST_ALPHA)?.unwrap_or(u16::MAX);
            }
            if tr.overrun() {
                return Err(Error::Corrupt);
            }
        }
        let dummy = freqs.len() as u16;
        freqs.push(vec![rans::PROB_SCALE]);
        for cx in ctx.iter_mut() {
            for t in cx.iter_mut() {
                if *t == u16::MAX {
                    *t = dummy;
                }
            }
        }
        for ci in unused {
            ctx[ci] = [dummy; PER_CLASS];
        }
        Ok(DecoderState {
            tables: Tables::new(&freqs),
            ctx,
            dec: Decoder::new(st),
            raw: BitR::new(raw),
            layout: BitR::new(layout),
            layout_bits: layout.len().saturating_mul(8),
            headers: BitReader::new(headers),
            recipe: Recipe::default(),
        })
    }

    /// Rebuilds the next stream (class `c`, `plain` bytes of data, pixels as given to
    /// [`Encoder::add`]): returns the DEFLATE bytes and the data.
    // Not inlined into the caller: inlined into the item loop, it was compiled about 5%
    // slower.
    #[inline(never)]
    pub fn next(&mut self, c: Class, plain: usize, px: Option<Pixels>) -> Result<(Vec<u8>, Vec<u8>)> {
        if px.is_some_and(|p| !tiles(p)) {
            return Err(Error::Corrupt);
        }
        let l = &mut self.layout;
        let nblocks = l.gamma()? as usize;
        if nblocks > self.layout_bits / 3 {
            return Err(Error::Corrupt);
        }
        let std_last = l.bits(1) == 1;
        // Per block: stored length, or token count.
        let mut blocks = Vec::with_capacity(nblocks);
        let mut spans = Vec::with_capacity(nblocks);
        let mut prev_count = 0usize;
        let mut ntok = 0usize;
        for i in 0..nblocks {
            let last = if std_last { i + 1 == nblocks } else { l.bits(1) == 1 };
            let body = match l.bits(2) {
                0 => {
                    let pad = l.bits(8) as u8;
                    let len = l.bits(16) as u16;
                    spans.push((true, len as usize));
                    Body::Stored { pad, len }
                }
                t @ (1 | 2) => {
                    let count = if l.bits(1) == 1 { prev_count } else { l.gamma()? as usize };
                    if count > plain {
                        return Err(Error::Corrupt);
                    }
                    prev_count = count;
                    ntok += count;
                    spans.push((false, count));
                    if t == 1 {
                        Body::Fixed { tokens: vec![] }
                    } else {
                        Body::Dynamic { header: DynHeader::default(), tokens: vec![] }
                    }
                }
                _ => return Err(Error::Corrupt),
            };
            blocks.push(Block { last, body });
        }
        let tail_pad = l.bits(8) as u8;
        let nalts = l.gamma()? as usize;
        if nalts > ntok {
            return Err(Error::Corrupt);
        }
        let mut alts = Vec::with_capacity(nalts);
        let mut last = 0usize;
        for k in 0..nalts {
            let d = l.gamma()? as usize;
            // Increasing: each match is listed once.
            if k > 0 && d == 0 {
                return Err(Error::Corrupt);
            }
            last += d;
            alts.push(last);
        }
        if l.overrun() || ntok > plain {
            return Err(Error::Corrupt);
        }

        let slot = if px.is_some() { PIXEL } else { c as usize };
        let mut data = vec![0u8; plain + SLACK];
        let mut tokens: Vec<Token> = Vec::with_capacity(ntok);
        let mut freqs = Vec::with_capacity(nblocks);
        let mut dec = self.dec;
        let mut cur = Cursor {
            tb: &self.tables,
            cx: &self.ctx[slot],
            dec: &mut dec,
            raw: &mut self.raw,
            data: &mut data,
            tokens: &mut tokens,
            freqs: &mut freqs,
            o: 0,
            plain,
        };
        match px {
            None => cur.blocks(&spans, &mut Prev(0))?,
            Some(px) => {
                cur.blocks(&spans, &mut Pix { px, col: 0, ch: 0, after: false, last: Cell::new((usize::MAX, 0)) })?
            }
        }
        let o = cur.o;
        let raw = &mut self.raw;
        self.dec = dec;
        if o != plain || raw.overrun() {
            return Err(Error::Corrupt);
        }
        data.truncate(plain);
        let mut starts = Vec::with_capacity(nblocks + 1);
        let mut ti = 0;
        for &(stored, n) in &spans {
            starts.push(ti);
            if !stored {
                ti += n;
            }
        }
        starts.push(ti);
        for &i in &alts {
            match tokens.get_mut(i) {
                Some(Token::Match { len: 258, alt258, .. }) => *alt258 = true,
                _ => return Err(Error::Corrupt),
            }
            let b = starts.partition_point(|&s| s <= i) - 1;
            freqs[b].0[285] -= 1;
            freqs[b].0[284] += 1;
        }
        for (b, (lf, df)) in blocks.iter_mut().zip(&freqs) {
            if let Body::Dynamic { header, .. } = &mut b.body {
                *header =
                    header::read_freqs(&mut self.headers, lf, df, &mut self.recipe).map_err(|_| Error::Corrupt)?;
            }
        }
        let stream = Stream { blocks, tail_pad, trailing: vec![] };
        let out = aqr_flate::write::write(&stream, &starts, &tokens, &data).map_err(|_| Error::Corrupt)?;
        Ok((out, data))
    }

    /// Whether every stream was consumed exactly.
    pub fn finished(&self) -> bool {
        self.dec.finished()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes() {
        for d in 1..=32768u32 {
            let c = dist_code(d);
            assert!(DIST_BASE[c] as u32 <= d && (c == 29 || d < DIST_BASE[c + 1] as u32), "{d}");
        }
        for l in 3..=258u16 {
            let c = LEN_CODE[l as usize] as usize;
            assert!(LEN_BASE[c] <= l && (c == 28 || l < LEN_BASE[c + 1]) || l == 258 && c == 28, "{l}");
        }
    }

    #[test]
    fn roundtrip() {
        let mut text = vec![];
        for i in 0..20000 {
            text.extend_from_slice(format!("{} {} Td ({}) Tj\n", i % 97, i * 7 % 13, i * i % 1000).as_bytes());
        }
        let z = zstd::bulk::compress(&text, 1).unwrap();
        let _ = z;
        let raw = flate_zoo_like(&text);
        let f = aqr_flate::inflate::parse(&raw).unwrap();
        let mut e = Encoder::default();
        e.add(Class::Content, &f, None).unwrap();
        e.add(Class::Font, &f, None).unwrap();
        let px = Some(Pixels { bpp: 3, stride: 81 });
        e.add(Class::Image, &f, px).unwrap();
        let out = e.finish().0;
        let mut d = DecoderState::new(parts(&out)).unwrap();
        assert_eq!(d.next(Class::Content, text.len(), None).unwrap().0, raw);
        assert_eq!(d.next(Class::Font, text.len(), None).unwrap().0, raw);
        assert_eq!(d.next(Class::Image, text.len(), px).unwrap().0, raw);
        assert!(d.finished());
    }

    fn parts(p: &[Vec<u8>; PARTS]) -> [&[u8]; PARTS] {
        p.each_ref().map(|v| &v[..])
    }

    /// `a`, then a match of 258 from 1 back sent as 284 + 31, in one fixed block.
    fn alt258() -> (Vec<u8>, Vec<u8>) {
        let data = vec![b'a'; 259];
        let tokens = vec![Token::Lit(b'a'), Token::Match { len: 258, dist: 1, alt258: true }];
        let st = Stream { blocks: vec![Block { last: true, body: Body::Fixed { tokens } }], ..Default::default() };
        (aqr_flate::deflate::write(&st, &data).unwrap(), data)
    }

    /// The layout of [`alt258`] with the given indices listed as sent as 284 + 31.
    fn alt258_layout(tail_pad: u8, alts: &[u32]) -> Vec<u8> {
        let mut w = BitW::default();
        w.gamma(1);
        w.bits(1, 1);
        w.bits(2, 1);
        w.bits(1, 0);
        w.gamma(2);
        w.bits(8, tail_pad as u32);
        w.gamma(alts.len() as u32);
        let mut last = 0;
        for &i in alts {
            w.gamma(i - last);
            last = i;
        }
        w.finish()
    }

    /// A match listed twice as sent as 284 + 31 is invalid (it was counted off twice).
    #[test]
    fn alt258_listed_twice() {
        let (raw, data) = alt258();
        let f = aqr_flate::inflate::parse(&raw).unwrap();
        let mut e = Encoder::default();
        e.add(Class::Content, &f, None).unwrap();
        let mut p = e.finish().0;
        assert_eq!(p[0], alt258_layout(f.stream.tail_pad, &[1]));
        assert_eq!(DecoderState::new(parts(&p)).unwrap().next(Class::Content, data.len(), None).unwrap().0, raw);
        p[0] = alt258_layout(f.stream.tail_pad, &[1, 1]);
        assert!(DecoderState::new(parts(&p)).unwrap().next(Class::Content, data.len(), None).is_err());
    }

    /// Length 258 is length code 28; code 27 with extra bits 31 is invalid.
    #[test]
    fn len258_from_code27() {
        // Slot 0 at the coarsest resolution: one literal table (0) and distance table 7 (1).
        let mut lit = vec![0u32; LIT_ALPHA];
        lit[b'a' as usize] = 1;
        lit[256 + 27] = 1;
        let mut dist = vec![0u32; DIST_ALPHA];
        dist[0] = 1;
        let mut tw = BitW::default();
        tw.bits(1, 1);
        tw.bits(2, 3);
        tw.bits(1, 1);
        let mut freqs = vec![rans::write_table(&mut tw, &lit)];
        for k in 0..DIST_CTX {
            tw.bits(1, (k == 7) as u32);
            if k == 7 {
                freqs.push(rans::write_table(&mut tw, &dist));
            }
        }
        for _ in 1..SLOTS {
            tw.bits(1, 0);
        }
        let syms = rans::encode(&[(0, b'a' as u16), (0, 256 + 27), (1, 0)], &freqs);
        let mut raw = BitW::default();
        raw.bits(5, 31);
        let p = [alt258_layout(0, &[]), tw.finish(), syms, raw.finish(), vec![]];
        assert!(DecoderState::new(parts(&p)).unwrap().next(Class::Content, 259, None).is_err());
    }

    /// Pixel rows must hold whole pixels: the channel is the position modulo `bpp`.
    #[test]
    fn pixel_rows_whole() {
        let data: Vec<u8> = (0..400u32).map(|i| (i * i / 7) as u8).collect();
        let raw = flate_zoo_like(&data);
        let f = aqr_flate::inflate::parse(&raw).unwrap();
        let px = Some(Pixels { bpp: 2, stride: 5 });
        let mut e = Encoder::default();
        if e.add(Class::Image, &f, px).is_ok() {
            let out = e.finish().0;
            let back = DecoderState::new(parts(&out)).unwrap().next(Class::Image, data.len(), px);
            assert_eq!(back.map(|r| r.0), Ok(raw));
        }
    }

    /// A DEFLATE stream with fixed and stored blocks, built by hand from greedy matches.
    fn flate_zoo_like(data: &[u8]) -> Vec<u8> {
        let mut st = Stream::default();
        let mut tokens = vec![];
        let mut i = 0;
        let mut last: std::collections::HashMap<&[u8], usize> = Default::default();
        while i < data.len() {
            let key = &data[i..(i + 4).min(data.len())];
            let m = last.get(key).copied().filter(|&j| i - j <= 32768 && key.len() == 4);
            last.insert(key, i);
            if let Some(j) = m {
                let mut l = 0;
                while i + l < data.len() && l < 258 && data[j + l] == data[i + l] {
                    l += 1;
                }
                tokens.push(Token::Match { len: l as u16, dist: (i - j) as u16, alt258: false });
                i += l;
            } else {
                tokens.push(Token::Lit(data[i]));
                i += 1;
            }
        }
        st.blocks.push(Block { last: false, body: Body::Stored { pad: 0, len: 0 } });
        st.blocks.push(Block { last: true, body: Body::Fixed { tokens } });
        aqr_flate::deflate::write(&st, data).unwrap()
    }
}
