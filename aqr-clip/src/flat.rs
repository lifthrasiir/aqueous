//! A model of tiles as whole pixels (all samples at once), for flat colours and line art: a
//! pixel is most often a copy of a neighbour, else one seen before, else new.
//!
//! Per pixel, with adaptive bits ([`aqr_common::arith`]):
//!
//! 1. whether it equals the left, the upper, the upper right and the upper left pixel, tried in
//!    turn (each distinct value once), in contexts of which neighbours equal which and how many
//!    pixels in a row were copies of the left one;
//! 2. else whether it is in a list of the pixels coded this way before (most recent first, up
//!    to [`LIST`]), and its place there (an Elias-gamma code of adaptive bits); with
//!    [`Escape::Med`] the encoder codes a listed pixel as a new one where that costs less;
//! 3. else its samples ([`Escape`]): the alpha first, then for colour G, B, R and the fourth
//!    byte, each as the difference to a prediction.
//!
//! With runs ([`Model::new`]), a tile first says whether it is all zeros or one pixel all over
//! (then coded as a new one), and where the row above equals the left pixel from above on for
//! [`RUN_LOOK`] pixels (in the first row, where the pixel before the left one equals it), the
//! pixels equal to it from there on in the row are coded at once: whether as many as the
//! stretch above (in the first row, up to the end of the row), else how many (an Elias-gamma
//! code). The pixel that ends a run is coded as above, the left one not tried.
//!
//! The state carries over from tile to tile, as the coder does: tiles are coded in order. A
//! tile's first row has no pixels above, and its first column the pixel above for the left one.

use crate::tile::{AREA, Layout, SIDE};
use aqr_common::arith::{ArithDecoder, ArithEncoder, Bit};

pub trait Coder {
    /// Whether bits come from the coder rather than from the encoder's pixels.
    const DECODES: bool;
    /// Codes `b` (the encoder's) with `m`, and gives the bit coded.
    fn bit(&mut self, m: &mut Bit, b: bool) -> bool;
}

pub struct Enc(pub ArithEncoder);

impl Coder for Enc {
    const DECODES: bool = false;
    #[inline]
    fn bit(&mut self, m: &mut Bit, b: bool) -> bool {
        self.0.encode(b, m.p1());
        m.update(b);
        b
    }
}

pub struct Dec<'a>(pub ArithDecoder<'a>);

impl Coder for Dec<'_> {
    const DECODES: bool = true;
    #[inline]
    fn bit(&mut self, m: &mut Bit, _: bool) -> bool {
        let b = self.0.decode(m.p1());
        m.update(b);
        b
    }
}

/// What the encoder's bits would cost, in bits, the models left as they are.
#[derive(Default)]
struct Cost(f32);

impl Coder for Cost {
    const DECODES: bool = false;
    #[inline]
    fn bit(&mut self, m: &mut Bit, b: bool) -> bool {
        let p1 = m.p1() as usize;
        self.0 += COST[(if b { p1 } else { 65536 - p1 }) >> 4];
        b
    }
}

/// Bits of a symbol of probability `(i + 0.5) / 4096`.
static COST: std::sync::LazyLock<[f32; 4096]> =
    std::sync::LazyLock::new(|| std::array::from_fn(|i| -((i as f32 + 0.5) / 4096.0).log2()));

/// Pixels of the row above that must equal the left one for a run.
const RUN_LOOK: usize = 4;

/// Pixels kept in the list.
pub const LIST: usize = 256;
/// Neighbours tried.
const CANDS: usize = 4;
/// Contexts of a copy: whether the pixel ends a run, four equalities of neighbours, then the
/// run of left copies in 4 buckets.
const COPY_CTX: usize = 2 * 16 * 4;
/// Contexts of a run: whether in the first row, whether the stretch above reaches the end of
/// the row, and whether the last run was as long as its stretch.
const RUN_CTX: usize = 8;
/// Bits of the Elias-gamma code: the unary part, and the bits below the leading one by length.
const GAMMA: usize = 12;

struct Gamma {
    unary: [Bit; GAMMA],
    low: [[Bit; GAMMA]; GAMMA],
}

impl Default for Gamma {
    fn default() -> Self {
        Gamma { unary: [Bit::default(); GAMMA], low: [[Bit::default(); GAMMA]; GAMMA] }
    }
}

/// Codes `v` (below 2^(GAMMA - 1) - 1).
fn gamma<C: Coder>(c: &mut C, m: &mut Gamma, v: usize) -> usize {
    let x = v + 1;
    let e = (usize::BITS - 1 - x.leading_zeros()) as usize;
    let mut k = 0;
    while k + 1 < GAMMA && !c.bit(&mut m.unary[k], k == e) {
        k += 1;
    }
    let mut y = 1;
    for j in (0..k).rev() {
        y = y << 1 | c.bit(&mut m.low[k][j], x >> j & 1 == 1) as usize;
    }
    y - 1
}

/// Codes byte `v` with a binary tree.
fn byte<C: Coder>(c: &mut C, t: &mut [Bit; 256], v: u8) -> u8 {
    let mut n = 1;
    for j in (0..8).rev() {
        n = n << 1 | c.bit(&mut t[n], v >> j & 1 == 1) as usize;
    }
    n as u8
}

/// How the samples of a new pixel are coded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Escape {
    /// The difference to the left pixel's sample, in a binary tree of adaptive bits by sample
    /// and (for colour samples) whether the alpha is 0.
    Left,
    /// As aqr-psd's model: the residual against MED of the left, upper and upper left samples
    /// (for B and R plus G's residual), its zigzag in an Elias-gamma code of adaptive bits by
    /// sample, whether the alpha is 0, the activity around it and (after G) G's residual.
    Med,
}

/// Contexts of [`Escape::Med`]: by sample, transparency, activity and G's residual.
const SLOTS: usize = 5;
const ACT: usize = 8;
const CROSS: usize = 4;

pub struct Model {
    escape: Escape,
    copy: Vec<[Bit; COPY_CTX]>,
    listed: [Bit; CANDS + 1],
    place: Gamma,
    /// [`Escape::Left`]'s trees.
    samples: Vec<[Bit; 256]>,
    /// [`Escape::Med`]'s codes.
    residuals: Vec<Gamma>,
    list: Vec<u64>,
    runs: bool,
    /// Whether a tile is all zeros, and whether it is one pixel.
    kind: [Bit; 2],
    /// Runs by whether in the first row and whether the last run was as long as its stretch
    /// above: that this one is, else its length.
    full: [Bit; RUN_CTX],
    length: Vec<Gamma>,
    last_full: bool,
}

#[inline]
fn med(a: u8, b: u8, c: u8) -> u8 {
    let (lo, hi) = (a.min(b), a.max(b));
    if c >= hi {
        lo
    } else if c <= lo {
        hi
    } else {
        a.wrapping_add(b).wrapping_sub(c)
    }
}

#[inline]
fn bucket(v: u32, n: usize) -> usize {
    ((32 - v.leading_zeros()) as usize).min(n - 1)
}

impl Model {
    /// A model coding new pixels by `escape`, with runs and tile kinds where `runs`.
    pub fn new(escape: Escape, runs: bool) -> Model {
        Model {
            escape,
            copy: vec![[Bit::default(); COPY_CTX]; CANDS],
            listed: [Bit::default(); CANDS + 1],
            place: Gamma::default(),
            samples: vec![[Bit::default(); 256]; 10],
            residuals: (0..SLOTS * 2 * ACT * CROSS).map(|_| Gamma::default()).collect(),
            list: Vec::with_capacity(LIST + 1),
            runs,
            kind: [Bit::default(); 2],
            full: [Bit::default(); RUN_CTX],
            length: (0..RUN_CTX).map(|_| Gamma::default()).collect(),
            last_full: false,
        }
    }

    /// Codes the samples of a new pixel `want` (the encoder's) with neighbours `nb`: left,
    /// upper, upper left, upper right.
    fn fresh<C: Coder>(&mut self, c: &mut C, l: Layout, want: u64, nb: [Option<u64>; 4]) -> u64 {
        let [w, n, nw, ne] = nb;
        let mut v = 0u64;
        let mut transparent = false;
        let sample = |p: u64, s: usize| (p >> (8 * s)) as u8;
        if self.escape == Escape::Left {
            let base = w.unwrap_or(0);
            for s in 0..l.samples() {
                let t = if s == 0 { 0 } else { s + 4 * transparent as usize };
                let b = sample(base, s);
                let d = byte(c, &mut self.samples[t], sample(want, s).wrapping_sub(b));
                let x = d.wrapping_add(b);
                if s == 0 && l == Layout::Color {
                    transparent = x == 0;
                }
                v |= (x as u64) << (8 * s);
            }
            return v;
        }
        // missing neighbours stand in for each other, else 0
        let (a, b, cc, d) = (w.or(n), n.or(w), nw.or(n).or(w), ne.or(n).or(w));
        let order: &[usize] = match l {
            Layout::Color => &[0, 2, 1, 3, 4],
            Layout::One => &[0],
            Layout::Two => &[0, 1],
        };
        let mut g: Option<i32> = None;
        for (slot, &s) in order.iter().enumerate() {
            let at = |p: Option<u64>| p.map_or(0, |p| sample(p, s));
            let (a, b, cc, d) = (at(a), at(b), at(cc), at(d));
            let colour = l == Layout::Color && s > 0;
            let mut pred = med(a, b, cc);
            if let (true, Some(r), 1 | 3) = (colour, g, s) {
                pred = (pred as i32 + r).clamp(0, 255) as u8;
            }
            let act = (a as i32 - cc as i32).unsigned_abs()
                + (b as i32 - cc as i32).unsigned_abs()
                + (d as i32 - b as i32).unsigned_abs();
            let cross = match (s, g) {
                (1 | 3 | 4, Some(r)) => bucket(r.unsigned_abs(), CROSS),
                _ => 0,
            };
            let t = ((slot * 2 + (colour && transparent) as usize) * ACT + bucket(act, ACT)) * CROSS + cross;
            let r = sample(want, s).wrapping_sub(pred) as i8;
            let z = ((r as i32) << 1 ^ (r as i32) >> 7) as u8;
            let z = gamma(c, &mut self.residuals[t], z as usize).min(255) as u32;
            let r = ((z >> 1) as i32 ^ -((z & 1) as i32)) as i8;
            let x = pred.wrapping_add(r as u8);
            if s == 0 && l == Layout::Color {
                transparent = x == 0;
            }
            if colour && s == 2 {
                g = Some(r as i32);
            }
            v |= (x as u64) << (8 * s);
        }
        v
    }

    /// Codes a tile of layout `l`: `px` holds the encoder's pixels, and the decoder's
    /// afterwards.
    pub fn tile<C: Coder>(&mut self, c: &mut C, l: Layout, px: &mut [u64]) {
        if self.runs {
            let first = px[0];
            if c.bit(&mut self.kind[0], !C::DECODES && px.iter().all(|&p| p == 0)) {
                px.fill(0);
                return;
            }
            if c.bit(&mut self.kind[1], !C::DECODES && px.iter().all(|&p| p == first)) {
                let v = self.fresh(c, l, first, [None; 4]);
                px.fill(v);
                return;
            }
        }
        let (mut run, mut i, mut broke) = (0usize, 0, false);
        while i < AREA {
            let (x, y) = (i % SIDE, i / SIDE);
            if self.runs && !broke && x > 0 {
                let w = px[i - 1];
                let left = SIDE - x;
                // the stretch of the row above equal to the left pixel, which must reach
                // RUN_LOOK pixels on (or the end of the row); the first row has none
                let above = if y > 0 {
                    px[i - SIDE..i - SIDE + left].iter().take_while(|&&p| p == w).count()
                } else if x > 1 && px[i - 2] == w {
                    left
                } else {
                    0
                };
                if above >= RUN_LOOK.min(left) && above > 0 {
                    let len = if C::DECODES { 0 } else { px[i..i + left].iter().take_while(|&&p| p == w).count() };
                    let t = (y > 0) as usize * 4 + (above == left) as usize * 2 + self.last_full as usize;
                    let full = c.bit(&mut self.full[t], len == above);
                    let len = if full { above } else { gamma(c, &mut self.length[t], len).min(left - 1) };
                    self.last_full = full;
                    px[i..i + len].fill(w);
                    i += len;
                    broke = len < left;
                    run = 0;
                    continue;
                }
            }
            let n = (y > 0).then(|| px[i - SIDE]);
            let w = if x > 0 { Some(px[i - 1]) } else { n };
            let nw = (x > 0 && y > 0).then(|| px[i - SIDE - 1]);
            let ne = (y > 0 && x + 1 < SIDE).then(|| px[i - SIDE + 1]);
            let eq = |a: Option<u64>, b: Option<u64>| a.is_some() && a == b;
            let r = match run {
                0 => 0,
                1..=2 => 1,
                3..=15 => 2,
                _ => 3,
            };
            let ctx = (broke as usize * 16
                + (eq(w, n) as usize
                    | (eq(n, ne) as usize) << 1
                    | (eq(w, nw) as usize) << 2
                    | (eq(nw, n) as usize) << 3))
                * 4
                + r;
            let want = px[i];
            let mut tried = [0u64; CANDS];
            let mut nt = 0;
            if broke {
                // (the left pixel, which ended the run's)
                tried[0] = px[i - 1];
                nt = 1;
                broke = false;
            }
            let mut got = None;
            for (k, cand) in [w, n, ne, nw].into_iter().enumerate() {
                let Some(v) = cand else { continue };
                if tried[..nt].contains(&v) {
                    continue;
                }
                tried[nt] = v;
                nt += 1;
                if c.bit(&mut self.copy[k][ctx], want == v) {
                    got = Some((k, v));
                    break;
                }
            }
            let v = match got {
                Some((k, v)) => {
                    run = if k == 0 { run + 1 } else { 0 };
                    v
                }
                None => {
                    run = 0;
                    // A pixel in the list is coded by its place there, or with Escape::Med as
                    // a new one where that is cheaper; the list keeps each pixel once.
                    let at = if C::DECODES { None } else { self.list.iter().position(|&p| p == want) };
                    let listed = at.is_some_and(|j| {
                        self.escape == Escape::Left || {
                            let (mut by_place, mut by_samples) = (Cost::default(), Cost::default());
                            let mut m = self.listed[nt];
                            by_place.bit(&mut m, true);
                            gamma(&mut by_place, &mut self.place, j);
                            by_samples.bit(&mut m, false);
                            self.fresh(&mut by_samples, l, want, [w, n, nw, ne]);
                            by_place.0 <= by_samples.0
                        }
                    });
                    let v = if !self.list.is_empty() && c.bit(&mut self.listed[nt], listed) {
                        // (a corrupt place stands for the last)
                        let j = gamma(c, &mut self.place, at.unwrap_or(0)).min(self.list.len() - 1);
                        self.list.remove(j)
                    } else {
                        let v = self.fresh(c, l, want, [w, n, nw, ne]);
                        if let Some(j) = self.list.iter().position(|&p| p == v) {
                            self.list.remove(j);
                        }
                        v
                    };
                    self.list.insert(0, v);
                    self.list.truncate(LIST);
                    v
                }
            };
            px[i] = v;
            i += 1;
        }
    }
}
