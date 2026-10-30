//! The model of an image's pixels, palette indices (`FORMAT.md` §5): first whether an index is
//! the left one's (most are, but in photographs), else the index bit by bit from the top, each
//! bit's probability a logistic mix of those of contexts of the neighbouring indices, their
//! colors and the same pixel of the image before, in tables of hashed slots. Integers only, so
//! that the module gives the same natively and as Wasm.

use aqr_common::arith::{ArithDecoder, ArithEncoder, Bit};

/// The side of the arithmetic coder.
pub trait Coder {
    /// Codes `v` (the encoder's) of probability `p1`/65536 of being 1, and gives the bit coded.
    fn code(&mut self, p1: u32, v: bool, cat: Cat) -> bool;

    /// Bits coded so far, in 1/65536 of a bit, where the side counts them (the encoder's).
    fn spent(&self) -> u64 {
        0
    }

    /// Codes `v` with the adaptive bit `b`.
    #[inline]
    fn bit(&mut self, b: &mut Bit, v: bool, cat: Cat) -> bool {
        let v = self.code(b.p1(), v, cat);
        b.update(v);
        v
    }

    /// `n` bits of probability 1/2, from the top.
    fn raw(&mut self, n: u32, v: u32, cat: Cat) -> u32 {
        let mut x = 0;
        for i in (0..n).rev() {
            x = x << 1 | self.code(1 << 15, v >> i & 1 != 0, cat) as u32;
        }
        x
    }

    /// `v < 2^32 - 1` as Elias gamma of `v + 1`: its bit length less 1 in unary under the
    /// adaptive bits `t` (the last one shared beyond), then the bits below the top one, raw.
    fn gamma(&mut self, t: &mut [Bit], v: u32, cat: Cat) -> Option<u32> {
        let x = v.wrapping_add(1);
        let len = u32::BITS - x.leading_zeros();
        let mut b = 0;
        while self.bit(&mut t[b.min(t.len() - 1)], b + 1 < len as usize, cat) {
            b += 1;
            if b == 31 {
                return None;
            }
        }
        let low = self.raw(b as u32, x, cat);
        Some((1 << b | low) - 1)
    }
}

/// What the record codes, for the statistics.
#[derive(Clone, Copy)]
#[repr(usize)]
pub enum Cat {
    /// Where the images are, their counts of pixels, sub-blocks, padding and what follows.
    Layout,
    Pixels,
    /// The codes given the pixels: the clear codes, the lengths and the codes chosen.
    Parse,
}
pub const CATS: usize = 3;

pub struct Enc {
    pub c: ArithEncoder,
    /// Bits coded by [`Cat`], in 1/65536 of a bit (by [`COST`]).
    cost: [u64; CATS],
}

/// The cost of a bit of probability `p / 65536`, in 1/65536 of a bit: what the encoder's statistics
/// count, rather than a logarithm for every bit.
static COST: std::sync::LazyLock<Vec<u32>> =
    std::sync::LazyLock::new(|| (0..65536).map(|p| (-(p.max(1) as f64 / 65536.0).log2() * 65536.0) as u32).collect());

impl Enc {
    pub fn new() -> Self {
        Enc { c: ArithEncoder::new(Vec::new()), cost: [0; CATS] }
    }

    /// Bits coded by [`Cat`].
    pub fn cost(&self) -> [f64; CATS] {
        self.cost.map(|c| c as f64 / 65536.0)
    }
}

impl Default for Enc {
    fn default() -> Self {
        Self::new()
    }
}

impl Coder for Enc {
    fn spent(&self) -> u64 {
        self.cost.iter().sum()
    }

    #[inline]
    fn code(&mut self, p1: u32, v: bool, cat: Cat) -> bool {
        self.cost[cat as usize] += COST[(if v { p1 } else { 65536 - p1 }).min(65535) as usize] as u64;
        self.c.encode(v, p1);
        v
    }
}

pub struct Dec<'a> {
    pub c: ArithDecoder<'a>,
}

impl Coder for Dec<'_> {
    #[inline]
    fn code(&mut self, p1: u32, _: bool, _: Cat) -> bool {
        self.c.decode(p1)
    }
}

/// squash(x) = 4096 / (1 + e^(-x/256)) at x = -2048, -1920, ..., 2048.
const SQUASH: [i32; 33] = [
    1, 2, 4, 6, 10, 17, 27, 45, 74, 120, 194, 311, 488, 747, 1102, 1546, 2048, 2550, 2994, 3349, 3608, 3785, 3902,
    3976, 4022, 4051, 4069, 4079, 4086, 4090, 4092, 4094, 4095,
];

/// The probability (12 bits) of the stretched `x`, interpolated in [`SQUASH`].
#[inline]
const fn squash(x: i32) -> i32 {
    if x > 2047 {
        return 4095;
    }
    if x < -2047 {
        return 1;
    }
    let w = x & 127;
    let i = ((x >> 7) + 16) as usize;
    (SQUASH[i] * (128 - w) + SQUASH[i + 1] * w + 64) >> 7
}

/// stretch(p) = ln(p / (1 - p)) of 12-bit probabilities: the inverse of [`squash`].
fn stretch_table() -> Vec<i16> {
    let mut t = vec![0i16; 4096];
    let mut pi = 0;
    for x in -2047..=2047 {
        let v = squash(x);
        t[pi..=v as usize].fill(x as i16);
        pi = v as usize + 1;
    }
    t[pi..].fill(2047);
    t
}

/// Contexts mixed, and their lanes (padded, so that the mixers' loops are of whole vectors).
pub const INPUTS: usize = 10;
const LANES: usize = 16;
/// Slots of a table at most (of the contexts of many values), and at least: a table is about 4
/// slots for a pixel.
const MAX_BITS: u32 = 21;
const MIN_BITS: u32 = 10;
/// The slots of the tables of the contexts of few values at most: the order 0 (the bit's node
/// alone, 2 buckets for an index of 8 bits) and W (257 values).
const SMALL_BITS: [u32; INPUTS] = [8, 18, 32, 32, 32, 32, 32, 32, 32, 32];
/// Uses after which a slot adapts no faster (the most its 4 bits count).
const LIMIT: usize = 15;
/// The mixers' weight sets: by the node of the bit in the tree of an index (the first mixer),
/// and by its depth and how many of the neighbours W, N, NW, NE differ (the second).
const NODES: usize = 256;
const SETS: usize = NODES + 9 * 4;
/// The mixers' rate of learning, over 2^14.
const RATE_MIX: i32 = 4;
/// The weights at first, and at most, over 2^16.
const W0: i32 = 6554;
const W_MAX: i32 = (1 << 19) - 1;

/// 65536 / (n + 1.5).
const RATE: [u32; LIMIT + 1] = {
    let mut r = [0; LIMIT + 1];
    let mut i = 0;
    while i <= LIMIT {
        r[i] = (65536 * 2 / (2 * i + 3)) as u32;
        i += 1;
    }
    r
};

/// A slot at first: a probability of 1/2, no uses.
const SLOT0: u16 = 2048 << 4;

/// No neighbour there.
const NONE: u32 = 256;

pub struct Model {
    /// stretch(p) of the probabilities as the slots hold them (exclusive-or [`SLOT0`]).
    stretch_slot: Vec<i16>,
    /// The tables of each context, one after another: slots of a 12-bit probability over a 4-bit
    /// count of its uses, each held exclusive-or [`SLOT0`] (its value at first), so that the
    /// tables start as zeros: pages of them never used are never touched.
    slots: Vec<u16>,
    /// Where each table starts, and its buckets (of 16 slots) less 1.
    base: [usize; INPUTS],
    mask: [u32; INPUTS],
    weights: Vec<[i32; LANES]>,
    /// The last image's indices on the screen, [`NONE`] where it did not cover.
    canvas: Vec<u16>,
    screen: (usize, usize),
}

/// Where an image is, for the model.
pub struct Place<'a> {
    /// Its colors: red, green and blue of each index (black for those beyond).
    pub palette: &'a [u8],
    pub left: usize,
    pub top: usize,
    pub width: usize,
    /// Bits of an index: the minimum code size.
    pub min: u8,
}

#[inline]
fn hash(x: u32, y: u32) -> u32 {
    (x ^ y.wrapping_mul(0x2f0b_4f1d)).wrapping_mul(0x9e37_79b1).rotate_left(13).wrapping_mul(0x85eb_ca6b)
}

/// Bits of each table for `pixels` pixels.
fn table_bits(pixels: u64) -> [u32; INPUTS] {
    let b = (64 - (pixels * 4).leading_zeros()).clamp(MIN_BITS, MAX_BITS);
    SMALL_BITS.map(|s| b.min(s))
}

/// The screen, where its canvas is not too large.
fn canvas_of(screen: (usize, usize)) -> (usize, usize) {
    if screen.0 * screen.1 <= 1 << 26 { screen } else { (0, 0) }
}

impl Model {
    /// A model for images of `pixels` pixels in all, on a screen of `screen` (width, height).
    pub fn new(pixels: u64, screen: (usize, usize)) -> Self {
        let bits = table_bits(pixels);
        let screen = canvas_of(screen);
        let mut base = [0; INPUTS];
        let mut at = 0;
        for (b, &n) in base.iter_mut().zip(&bits) {
            *b = at;
            at += 1 << n;
        }
        let mut w0 = [0; LANES];
        w0[..INPUTS].fill(W0);
        Model {
            stretch_slot: {
                let t = stretch_table();
                (0..4096).map(|p| t[p ^ (SLOT0 >> 4) as usize]).collect()
            },
            slots: vec![0; at],
            base,
            mask: bits.map(|b| (1 << (b - 4)) - 1),
            weights: vec![w0; SETS],
            canvas: vec![NONE as u16; screen.0 * screen.1],
            screen,
        }
    }

    /// Bytes of a model for `pixels` pixels on a screen of `screen`.
    pub fn bytes(pixels: u64, screen: (usize, usize)) -> u64 {
        let screen = canvas_of(screen);
        let slots: usize = table_bits(pixels).iter().map(|&b| 1 << b).sum();
        (slots * 2 + 2 * screen.0 * screen.1 + 4 * SETS * LANES + 8192) as u64
    }

    /// Codes a bit (`want`, the encoder's) of the slots `idx` and the mixers' weight sets `set1`
    /// and `set2`, and gives it.
    #[inline(never)]
    fn bit(&mut self, c: &mut impl Coder, idx: &[usize; INPUTS], set1: usize, set2: usize, want: bool) -> bool {
        let mut st = [0i32; LANES];
        for j in 0..INPUTS {
            // SAFETY: a slot is within its table's bucket (of 16 slots).
            st[j] = self.stretch_slot[(unsafe { *self.slots.get_unchecked(idx[j]) } >> 4) as usize] as i32;
        }
        let (w1, w2) = (&self.weights[set1], &self.weights[set2]);
        let (mut dot1, mut dot2) = (0i32, 0i32);
        for j in 0..LANES {
            dot1 += (st[j] * w1[j]) >> 10;
            dot2 += (st[j] * w2[j]) >> 10;
        }
        let (dot1, dot2) = ((dot1 >> 6).clamp(-4095, 4095), (dot2 >> 6).clamp(-4095, 4095));
        let (p1, p2) = (squash(dot1), squash(dot2));
        let pr = squash((dot1 + dot2) / 2).clamp(1, 4095);
        let v = c.code((pr as u32) << 4 | 8, want, Cat::Pixels);
        let target = if v { 65535 } else { 0 };
        // the weights, towards the bit
        let (e1, e2) = ((((v as i32) << 12) - p1) * RATE_MIX, (((v as i32) << 12) - p2) * RATE_MIX);
        for (set, e) in [(set1, e1), (set2, e2)] {
            let wt = &mut self.weights[set];
            for j in 0..LANES {
                wt[j] = (wt[j] + ((st[j] * e) >> 14)).clamp(-W_MAX, W_MAX);
            }
        }
        // the slots
        for &s in idx {
            // SAFETY: as above.
            let slot = unsafe { self.slots.get_unchecked_mut(s) };
            let (p0, n0) = (((*slot ^ SLOT0) >> 4) as i32, (*slot & 15) as usize);
            let p1 = (p0 + (((target / 16 - p0) * RATE[n0] as i32) >> 16)).clamp(1, 4095);
            *slot = ((p1 as u16) << 4 | (n0 + (n0 < LIMIT) as usize) as u16) ^ SLOT0;
        }
        v
    }

    /// Codes the indices `px` (the encoder's, or filled in) of an image at `at`, row after row.
    pub fn image(&mut self, c: &mut impl Coder, px: &mut [u8], at: &Place<'_>) {
        let w = at.width.max(1);
        let k = at.min as u32;
        let (sw, sh) = self.screen;
        let mut pal = [[0i32; 3]; 257];
        for (i, c) in at.palette.as_chunks::<3>().0.iter().take(256).enumerate() {
            pal[i] = [c[0] as i32, c[1] as i32, c[2] as i32];
        }
        let mut h = [0u32; INPUTS];
        let mut bucket = [0usize; INPUTS];
        let (mut x, mut y) = (0, 0);
        for i in 0..px.len() {
            // the neighbours: W, N, NW, NE, WW, NN
            let g = |dx: usize, dy: usize, right: bool| -> u32 {
                if y < dy || x < dx || right && x + 1 >= w { NONE } else { px[i - dy * w - dx + right as usize] as u32 }
            };
            let (wv, n, nw, ne) = (g(1, 0, false), g(0, 1, false), g(1, 1, false), g(0, 1, true));
            let (ww, nn) = (g(2, 0, false), g(0, 2, false));
            let (cx, cy) = (at.left + x, at.top + y);
            let p = if cx < sw && cy < sh { self.canvas[cy * sw + cx] as u32 } else { NONE };
            // the colors: the mean of the four neighbours (4 bits a channel), the gradient
            // W + N - NW (4 bits), the mean of N and NE (3 bits)
            let (cw, cn, cnw, cne) = (pal[wv as usize], pal[n as usize], pal[nw as usize], pal[ne as usize]);
            let (mut mean, mut grad, mut up) = (0u32, 0u32, 0u32);
            for ch in 0..3 {
                mean = mean << 4 | (((cw[ch] * 2 + cn[ch] * 2 + cnw[ch] + cne[ch]) / 6) >> 4) as u32;
                grad = grad << 4 | ((cw[ch] + cn[ch] - cnw[ch]).clamp(0, 255) >> 4) as u32;
                up = up << 3 | (((cn[ch] + cne[ch]) / 2) >> 5) as u32;
            }
            h[0] = 0;
            h[1] = hash(1, wv);
            h[2] = hash(hash(3, wv | n << 9), nw | ne << 9);
            h[3] = hash(hash(4, wv | ww << 9), n);
            h[4] = hash(hash(5, n | nn << 9), ne | nw << 9);
            h[5] = hash(hash(6, p), wv | n << 9);
            h[6] = hash(8, mean);
            h[7] = hash(hash(9, up), wv);
            h[8] = hash(10, grad);
            h[9] = hash(hash(11, up), n);
            let differ =
                (wv != n) as usize + (nw != wv && nw != n) as usize + (ne != wv && ne != n && ne != nw) as usize;
            let want = px[i] as u32;
            // the slots of a context are in buckets of 16 (of a cache line), the first of the
            // index's first 4 bits, the second of the next 4 under them; the first bucket's
            // slot 0 is whether the pixel is W's
            for j in 0..INPUTS {
                bucket[j] = self.base[j] + ((hash(h[j], 1) & self.mask[j]) << 4) as usize;
            }
            if wv != NONE && self.bit(c, &bucket, 0, NODES + 8 * 4 + differ, want == wv) {
                px[i] = wv as u8;
            } else {
                let mut node = 1u32;
                let mut sub = 1usize;
                for b in (0..k).rev() {
                    let depth = k - 1 - b;
                    if depth > 0 && depth.is_multiple_of(4) {
                        for j in 0..INPUTS {
                            bucket[j] = self.base[j] + ((hash(h[j], node) & self.mask[j]) << 4) as usize;
                        }
                        sub = 1;
                    }
                    let idx = bucket.map(|b| b + sub);
                    let set2 = NODES + depth.min(7) as usize * 4 + differ;
                    let v = self.bit(c, &idx, node as usize, set2, want >> b & 1 != 0);
                    node = node << 1 | v as u32;
                    sub = sub << 1 | v as usize;
                }
                px[i] = (node & ((1 << k) - 1)) as u8;
            }
            x += 1;
            if x == w {
                x = 0;
                y += 1;
            }
        }
        // the image on the screen, for the next one
        for (i, &v) in px.iter().enumerate() {
            let (cx, cy) = (at.left + i % w, at.top + i / w);
            if cx < sw && cy < sh {
                self.canvas[cy * sw + cx] = v as u16;
            }
        }
    }
}
