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
use aqr_flate::deflate::{Block, Body, DynHeader, Token};
use aqr_flate::header::{self, Recipe};
use aqr_flate::huffman::{DIST_BASE, DIST_EXTRA, LEN_BASE, LEN_EXTRA};
use aqr_flate::inflate::{Layout, Sink};
use aqr_flate::write::Writer;
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

/// Bytes of data a window keeps behind its position when it slides: a match goes at most
/// 32 KiB back, the pixel model a row, a pixel and a byte (MED of the byte before).
const WINDOW: usize = 1 << 15;
/// Bytes a window moves on by at least before it slides.
const STEP: usize = 1 << 18;

/// A stream's data as it is made, from `off` on: `data[..o]`, then room. It slides once it
/// holds `keep` bytes of history and `STEP` (or `keep`) more, the bytes that leave it summed
/// into the Adler-32. Positions are within the window: with `keep` bytes kept, a match and
/// the literal models read the same bytes as in the whole data, and the first bytes are only
/// ever at the start of the window. As an inflate sink, the data of a stream alone.
pub struct Window {
    data: Vec<u8>,
    off: usize,
    o: usize,
    keep: usize,
    adler: simd_adler32::Adler32,
}

impl Window {
    /// A window for data laid out as `px` if given.
    pub fn new(px: Option<Pixels>) -> Window {
        Window {
            data: vec![],
            off: 0,
            o: 0,
            keep: px.map_or(WINDOW, |p| WINDOW.max(p.stride.saturating_add(p.bpp + 1))),
            adler: simd_adler32::Adler32::new(),
        }
    }

    /// Room for `n` more bytes; whether the window slid first.
    #[inline(always)]
    fn room(&mut self, n: usize) -> bool {
        self.o + n + SLACK > self.data.len() && self.make_room(n)
    }

    #[cold]
    #[inline(never)]
    fn make_room(&mut self, n: usize) -> bool {
        let far = self.keep + STEP.max(self.keep);
        let slide = self.o >= far;
        if slide {
            let cut = self.o - self.keep;
            self.adler.write(&self.data[..cut]);
            self.data.copy_within(cut..self.o, 0);
            self.off += cut;
            self.o = self.keep;
        }
        // grown by doubling, up to what it holds before it slides
        let need = self.o + n + SLACK;
        if self.data.len() < need {
            self.data.resize(need.max((2 * self.data.len()).min(far + n + SLACK)), 0);
        }
        slide
    }

    /// Bytes of data so far.
    pub fn data_len(&self) -> usize {
        self.off + self.o
    }

    /// The Adler-32 of the data; the window is let go.
    pub fn finish(&mut self) -> u32 {
        self.adler.write(&self.data[..self.o]);
        self.data = vec![];
        self.adler.finish()
    }
}

impl Sink for Window {
    #[inline(always)]
    fn reserve(&mut self) {
        self.room(258);
    }

    #[inline(always)]
    fn lit(&mut self, b: u8) {
        self.data[self.o] = b;
        self.o += 1;
    }

    #[inline(always)]
    fn mat(&mut self, len: u16, dist: u16, _: bool) {
        copy_match(&mut self.data, self.o, dist as usize, len as usize);
        self.o += len as usize;
    }

    fn stored(&mut self, bytes: &[u8]) {
        self.room(bytes.len());
        self.data[self.o..self.o + bytes.len()].copy_from_slice(bytes);
        self.o += bytes.len();
    }
}

/// A stream for the token coder, made as it is parsed (an inflate sink, through a window of
/// its data): its symbols (stored blocks' bytes as literals), the extra bits of its matches
/// and the matches sent as 284 + 31. Nothing in it depends on the streams before, so that
/// streams can be made apart and added to the [`Encoder`] in order.
pub struct Part {
    slot: usize,
    px: Option<Pixels>,
    win: Window,
    syms: Vec<(u16, u16)>,
    raw: BitW,
    raw_bits: u64,
    /// Indices of the matches sent as 284 + 31.
    alts: Vec<usize>,
    tokens: usize,
    /// Whether a match came just before (pixels).
    after: bool,
    /// The pixel model's last position and MED.
    last: Cell<(usize, i32)>,
}

impl Part {
    /// A part of class `c`, an image coded as pixels if `px` is given (whose rows must hold
    /// whole pixels).
    pub fn new(c: Class, px: Option<Pixels>) -> Result<Part> {
        if px.is_some_and(|p| !tiles(p)) {
            return Err(Error::Corrupt);
        }
        Ok(Part {
            slot: if px.is_some() { PIXEL } else { c as usize },
            px,
            win: Window::new(px),
            syms: vec![],
            raw: BitW::default(),
            raw_bits: 0,
            alts: vec![],
            tokens: 0,
            after: false,
            last: Cell::new((usize::MAX, 0)),
        })
    }

    /// Bytes of data so far.
    pub fn data_len(&self) -> usize {
        self.win.data_len()
    }

    /// The Adler-32 of the data, once all of it is in; the window is let go.
    pub fn finish(&mut self) -> u32 {
        self.win.finish()
    }

    /// The fine literal context of the next byte, and the byte it is coded against.
    #[inline(always)]
    fn ctx(&self) -> (usize, u8) {
        let (d, p) = (&self.win.data[..], self.win.o);
        match self.px {
            None => (if p > 0 { d[p - 1] } else { 0 } as usize, 0),
            Some(px) => {
                let col = self.win.data_len() % px.stride;
                let (pred, t) = pixel_ctx(d, p, col, col % px.bpp, self.after, px, &self.last);
                (t, pred)
            }
        }
    }

    #[inline(always)]
    fn sym(&mut self, t: usize, s: usize) {
        self.syms.push(((self.slot * PER_CLASS + t) as u16, s as u16));
    }

    #[inline(always)]
    fn room(&mut self, n: usize) {
        if self.win.room(n) {
            self.last.set((usize::MAX, 0));
        }
    }
}

impl Sink for Part {
    #[inline(always)]
    fn reserve(&mut self) {
        self.room(258);
    }

    #[inline(always)]
    fn lit(&mut self, b: u8) {
        let (t, pred) = self.ctx();
        self.after = false;
        self.sym(t, b.wrapping_sub(pred) as usize);
        self.win.lit(b);
        self.tokens += 1;
    }

    #[inline(always)]
    fn mat(&mut self, len: u16, dist: u16, alt258: bool) {
        let prev = self.ctx().0;
        self.after = true;
        let lc = LEN_CODE[len as usize] as usize;
        let dc = dist_code(dist as u32);
        self.sym(prev, 256 + lc);
        self.raw.bits(LEN_EXTRA[lc] as u32, (len - LEN_BASE[lc]) as u32);
        self.sym(LIT_CTX + lc.min(DIST_CTX - 1), dc);
        self.raw.bits(DIST_EXTRA[dc] as u32, (dist - DIST_BASE[dc]) as u32);
        self.raw_bits += (LEN_EXTRA[lc] + DIST_EXTRA[dc]) as u64;
        if alt258 {
            self.alts.push(self.tokens);
        }
        self.win.mat(len, dist, alt258);
        self.tokens += 1;
    }

    fn stored(&mut self, bytes: &[u8]) {
        self.room(bytes.len());
        for &b in bytes {
            let (t, pred) = self.ctx();
            self.after = false;
            self.sym(t, b.wrapping_sub(pred) as usize);
            self.win.lit(b);
        }
    }
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
    /// Adds a stream: its part, and its layout as parsed (with no trailing bytes).
    pub fn add(&mut self, p: Part, l: &Layout) -> Result<()> {
        if !l.stream.trailing.is_empty() {
            return Err(Error::Corrupt);
        }
        let ci = p.slot;
        if self.counts[ci].is_empty() {
            self.counts[ci] = vec![0; PER_CLASS * STRIDE];
        }
        // Layout.
        let w = &mut self.layout;
        let blocks = &l.stream.blocks;
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
                    let count = l.starts[i + 1] - l.starts[i];
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
        w.bits(8, l.stream.tail_pad as u32);
        w.gamma(p.alts.len() as u32);
        let mut last = 0usize;
        for &i in &p.alts {
            w.gamma((i - last) as u32);
            last = i;
        }
        // Headers.
        for (b, (lf, df)) in blocks.iter().zip(&l.stats.freqs) {
            if let Body::Dynamic { header: h, .. } = &b.body {
                header::write(&mut self.headers, lf, df, h, &mut self.recipe);
            }
        }
        // Symbols and raw bits.
        let counts = &mut self.counts[ci];
        let base = ci * PER_CLASS;
        for &(t, s) in &p.syms {
            counts[(t as usize - base) * STRIDE + s as usize] += 1;
        }
        if self.syms.is_empty() {
            self.syms = p.syms;
        } else {
            self.syms.extend_from_slice(&p.syms);
        }
        self.raw.append(&p.raw);
        self.raw_bits[ci] += p.raw_bits;
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
                    .as_chunks::<LIT_ALPHA>()
                    .0
                    .iter()
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
        let mut syms = self.syms;
        for s in &mut syms {
            s.0 = remap[s.0 as usize];
        }
        let st = rans::encode(&syms, &freqs);
        drop(syms);
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
    /// The positions given from now on are of a window that has slid.
    fn rebase(&mut self) {}
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
    fn rebase(&mut self) {
        self.last.set((usize::MAX, 0));
    }
}

/// Tokens decoded at a time, the window having room for all their data.
const CHUNK: usize = 1 << 10;

/// One stream's decoding state, its data in a window.
struct Cursor<'s, 'a> {
    tb: &'s Tables,
    cx: &'s [u16; PER_CLASS],
    dec: Decoder<'a>,
    raw: &'s mut BitR<'a>,
    win: Window,
    plain: usize,
}

impl<'a> Cursor<'_, 'a> {
    /// Room for `n` more bytes (as far as the data goes).
    fn room<M: LitModel>(&mut self, n: usize, m: &mut M) {
        if self.win.room(n.min(self.plain - self.win.data_len())) {
            m.rebase();
        }
    }

    /// Decodes a stored block of `n` bytes, coded as literals; returns where it starts.
    fn stored<M: LitModel>(&mut self, n: usize, m: &mut M) -> Result<usize> {
        if self.win.data_len() + n > self.plain {
            return Err(Error::Corrupt);
        }
        self.room(n, m);
        let (tb, cx) = (self.tb, self.cx);
        let mut dec = self.dec;
        let start = self.win.o;
        let data = &mut self.win.data[..];
        for o in start..start + n {
            let (pred, t) = m.ctx(data, o);
            let s = dec.get(tb, cx[t] as usize);
            if s >= 256 {
                return Err(Error::Corrupt);
            }
            let b = (s as u8).wrapping_add(pred);
            data[o] = b;
            m.lit(b);
        }
        self.dec = dec;
        self.win.o = start + n;
        Ok(start)
    }

    /// Decodes a block of `n` tokens into `tokens` (which it clears), with the block's symbol
    /// frequencies.
    #[inline(always)]
    fn tokens<M: LitModel>(&mut self, n: usize, m: &mut M, tokens: &mut Vec<Token>) -> Result<Freqs> {
        let mut lf = [0u32; 288];
        let mut df = [0u32; 32];
        lf[256] = 1;
        tokens.clear();
        tokens.reserve(n);
        let mut left = n;
        while left > 0 {
            let k = left.min(CHUNK);
            left -= k;
            self.room(k * 258, m);
            let (tb, cx) = (self.tb, self.cx);
            let mut dec = self.dec;
            let plain = self.plain - self.win.off;
            let mut o = self.win.o;
            let data = &mut self.win.data[..];
            // The tokens go straight into the buffer (reserved above).
            let tp = tokens.as_mut_ptr();
            let mut ti = tokens.len();
            for _ in 0..k {
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
                // SAFETY: `ti` stays below `n`, the room reserved.
                unsafe { tp.add(ti).write(tok) };
                ti += 1;
            }
            // SAFETY: the first `ti` tokens were written.
            unsafe { tokens.set_len(ti) };
            self.dec = dec;
            self.win.o = o;
        }
        Ok((lf, df))
    }

    /// The Adler-32 of the data, once all of it is in.
    fn finish(mut self) -> Result<(Decoder<'a>, u32)> {
        if self.win.data_len() != self.plain || self.raw.overrun() {
            return Err(Error::Corrupt);
        }
        Ok((self.dec, self.win.finish()))
    }
}

/// A block's symbol frequencies, as [`header::read_freqs`] takes them.
type Freqs = ([u32; 288], [u32; 32]);

/// Rebuilds the blocks of a stream through `cur` into `w`, with the literal model `m`: each
/// block is written once its tokens are decoded, the matches at `alts` (token indices) sent
/// as 284 + 31, and the dynamic Huffman headers read against the blocks' frequencies.
fn rebuild<M: LitModel>(
    cur: &mut Cursor,
    m: &mut M,
    blocks: &mut [(Block, usize)],
    alts: &[usize],
    headers: &mut BitReader,
    recipe: &mut Recipe,
    w: &mut Writer,
) -> Result<()> {
    let mut tokens = Vec::new();
    let (mut t0, mut alts) = (0, alts.iter().copied().peekable());
    for (b, n) in blocks {
        if let Body::Stored { .. } = b.body {
            let at = cur.stored(*n, m)?;
            w.block_of(b, &[], &cur.win.data[at..at + *n]).map_err(|_| Error::Corrupt)?;
            continue;
        }
        let (mut lf, df) = cur.tokens(*n, m, &mut tokens)?;
        while let Some(i) = alts.next_if(|&i| i < t0 + *n) {
            match tokens.get_mut(i - t0) {
                Some(Token::Match { len: 258, alt258, .. }) => *alt258 = true,
                _ => return Err(Error::Corrupt),
            }
            lf[285] -= 1;
            lf[284] += 1;
        }
        if let Body::Dynamic { header, .. } = &mut b.body {
            *header = header::read_freqs(headers, &lf, &df, recipe).map_err(|_| Error::Corrupt)?;
        }
        w.block_of(b, &tokens, &[]).map_err(|_| Error::Corrupt)?;
        t0 += *n;
    }
    // every listed match is one
    if alts.next().is_some() {
        return Err(Error::Corrupt);
    }
    Ok(())
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
    /// [`Encoder::add`]) block by block, its DEFLATE bytes appended to `out`; returns the
    /// Adler-32 of its data.
    // Not inlined into the caller: inlined into the item loop, it was compiled about 5%
    // slower.
    #[inline(never)]
    pub fn next(&mut self, c: Class, plain: usize, px: Option<Pixels>, out: &mut Vec<u8>) -> Result<u32> {
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
        let mut prev_count = 0usize;
        let mut ntok = 0usize;
        for i in 0..nblocks {
            let last = if std_last { i + 1 == nblocks } else { l.bits(1) == 1 };
            let (body, n) = match l.bits(2) {
                0 => {
                    let pad = l.bits(8) as u8;
                    let len = l.bits(16) as u16;
                    (Body::Stored { pad, len }, len as usize)
                }
                t @ (1 | 2) => {
                    let count = if l.bits(1) == 1 { prev_count } else { l.gamma()? as usize };
                    if count > plain {
                        return Err(Error::Corrupt);
                    }
                    prev_count = count;
                    ntok += count;
                    if t == 1 {
                        (Body::Fixed { tokens: vec![] }, count)
                    } else {
                        (Body::Dynamic { header: DynHeader::default(), tokens: vec![] }, count)
                    }
                }
                _ => return Err(Error::Corrupt),
            };
            blocks.push((Block { last, body }, n));
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
        let mut cur = Cursor {
            tb: &self.tables,
            cx: &self.ctx[slot],
            dec: self.dec,
            raw: &mut self.raw,
            win: Window::new(px),
            plain,
        };
        // (a writer of its own: it reserves room for a block's tokens at the worst)
        let mut w = Writer::new(1 << 16);
        let (headers, recipe) = (&mut self.headers, &mut self.recipe);
        match px {
            None => rebuild(&mut cur, &mut Prev(0), &mut blocks, &alts, headers, recipe, &mut w)?,
            Some(px) => {
                let mut m = Pix { px, col: 0, ch: 0, after: false, last: Cell::new((usize::MAX, 0)) };
                rebuild(&mut cur, &mut m, &mut blocks, &alts, headers, recipe, &mut w)?
            }
        }
        let (dec, adler) = cur.finish()?;
        self.dec = dec;
        out.extend_from_slice(&w.finish(tail_pad, &[]).map_err(|_| Error::Corrupt)?);
        Ok(adler)
    }

    /// Whether every stream was consumed exactly.
    pub fn finished(&self) -> bool {
        self.dec.finished()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aqr_flate::deflate::Stream;

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
        let mut e = Encoder::default();
        add(&mut e, &raw, Class::Content, None).unwrap();
        add(&mut e, &raw, Class::Font, None).unwrap();
        let px = Some(Pixels { bpp: 3, stride: 81 });
        add(&mut e, &raw, Class::Image, px).unwrap();
        let out = e.finish().0;
        let mut d = DecoderState::new(parts(&out)).unwrap();
        assert_eq!(rebuilt(&mut d, Class::Content, text.len(), None).unwrap(), raw);
        assert_eq!(rebuilt(&mut d, Class::Font, text.len(), None).unwrap(), raw);
        assert_eq!(rebuilt(&mut d, Class::Image, text.len(), px).unwrap(), raw);
        assert!(d.finished());
    }

    /// Adds `raw`, parsed, of class `c` (pixels `px`).
    fn add(e: &mut Encoder, raw: &[u8], c: Class, px: Option<Pixels>) -> Result<()> {
        let mut p = Part::new(c, px)?;
        let l = aqr_flate::inflate::parse_into(raw, usize::MAX, &mut p).map_err(|_| Error::Corrupt)?;
        e.add(p, &l)
    }

    /// The next stream of `d`, rebuilt.
    fn rebuilt(d: &mut DecoderState, c: Class, plain: usize, px: Option<Pixels>) -> Result<Vec<u8>> {
        let mut out = vec![];
        d.next(c, plain, px, &mut out).map(|_| out)
    }

    /// Streams longer than the decoder's window, which slides: stored blocks among the
    /// others, blocks of more tokens than are decoded at a time, pixels with wide rows.
    #[test]
    fn window_slides() {
        let mut x = 1u32;
        let data: Vec<u8> = (0..12 * STEP + 12345)
            .map(|i| {
                x = x.wrapping_mul(1103515245).wrapping_add(12345);
                if x >> 28 < 3 { (x >> 16) as u8 } else { b"0 1 2 Td (abc) Tj\n"[i % 18] }
            })
            .collect();
        let mut st = Stream::default();
        let (mut i, mut tokens) = (0, vec![]);
        let at = 9 * STEP + 777;
        for (start, end, stored) in [(0, at, false), (at, at + 1000, true), (at + 1000, data.len(), false)] {
            if stored {
                st.blocks.push(Block { last: false, body: Body::Stored { pad: 0, len: (end - start) as u16 } });
                i = end;
                continue;
            }
            // greedy matches, as in `flate_zoo_like`, cut into blocks by the stored one
            let mut last: std::collections::HashMap<&[u8], usize> = Default::default();
            while i < end {
                let key = &data[i..(i + 4).min(end)];
                let m = last.get(key).copied().filter(|&j| i - j <= 32768 && key.len() == 4);
                last.insert(key, i);
                if let Some(j) = m {
                    let mut l = 0;
                    while i + l < end && l < 258 && data[j + l] == data[i + l] {
                        l += 1;
                    }
                    tokens.push(Token::Match { len: l as u16, dist: (i - j) as u16, alt258: false });
                    i += l;
                } else {
                    tokens.push(Token::Lit(data[i]));
                    i += 1;
                }
            }
            st.blocks
                .push(Block { last: end == data.len(), body: Body::Fixed { tokens: std::mem::take(&mut tokens) } });
        }
        let raw = aqr_flate::deflate::write(&st, &data).unwrap();
        let mut e = Encoder::default();
        add(&mut e, &raw, Class::Content, None).unwrap();
        let px = Some(Pixels { bpp: 3, stride: 3 * 15000 });
        add(&mut e, &raw, Class::Image, px).unwrap();
        let out = e.finish().0;
        let mut d = DecoderState::new(parts(&out)).unwrap();
        for px in [None, px] {
            let mut back = b"zlib".to_vec();
            let sum = d.next(if px.is_some() { Class::Image } else { Class::Content }, data.len(), px, &mut back);
            assert_eq!(sum, Ok(crate::flate::adler32(&data)));
            assert!(back[4..] == raw[..] && &back[..4] == b"zlib");
        }
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
        let tail_pad = aqr_flate::inflate::parse(&raw).unwrap().stream.tail_pad;
        let mut e = Encoder::default();
        add(&mut e, &raw, Class::Content, None).unwrap();
        let mut p = e.finish().0;
        assert_eq!(p[0], alt258_layout(tail_pad, &[1]));
        assert_eq!(rebuilt(&mut DecoderState::new(parts(&p)).unwrap(), Class::Content, data.len(), None).unwrap(), raw);
        p[0] = alt258_layout(tail_pad, &[1, 1]);
        assert!(rebuilt(&mut DecoderState::new(parts(&p)).unwrap(), Class::Content, data.len(), None).is_err());
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
        assert!(rebuilt(&mut DecoderState::new(parts(&p)).unwrap(), Class::Content, 259, None).is_err());
    }

    /// Pixel rows must hold whole pixels: the channel is the position modulo `bpp`.
    #[test]
    fn pixel_rows_whole() {
        let data: Vec<u8> = (0..400u32).map(|i| (i * i / 7) as u8).collect();
        let raw = flate_zoo_like(&data);
        let px = Some(Pixels { bpp: 2, stride: 5 });
        let mut e = Encoder::default();
        if add(&mut e, &raw, Class::Image, px).is_ok() {
            let out = e.finish().0;
            let back = rebuilt(&mut DecoderState::new(parts(&out)).unwrap(), Class::Image, data.len(), px);
            assert_eq!(back, Ok(raw));
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
