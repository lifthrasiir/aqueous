//! The pixel model of integer images: each pixel predicted from its neighbours, its residual
//! coded by adaptive binary arithmetic coding as its length in bits (in a context of the
//! neighbourhood's activity), its first bit after the leading one, and the bits below as they
//! are, in a stream of their own (FORMAT.md, "The pixel model").
//!
//! Pixels are numbers of 8, 16 or 32 bits, as their bits (`u32`, the bits above zero): signed
//! or not, they are taken as such for prediction, and the residual is wrapped to their bits.
//! The image is rows of `width` pixels; pixels marked as known are not coded but are taken as
//! neighbours and go through the model all the same.

use crate::bits;
use aqr_common::arith::{ArithDecoder, ArithEncoder, Bit};

/// The numbers of an image.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shape {
    pub width: usize,
    /// Bits of a pixel: 8, 16 or 32.
    pub bits: u32,
    pub signed: bool,
}

impl Shape {
    pub fn valid(&self) -> bool {
        self.width > 0 && matches!(self.bits, 8 | 16 | 32)
    }

    #[inline(always)]
    pub fn value(&self, x: u32) -> i64 {
        if self.signed { ((x << (32 - self.bits)) as i32 >> (32 - self.bits)) as i64 } else { x as i64 }
    }

    #[inline(always)]
    pub fn mask(&self) -> u32 {
        u32::MAX >> (32 - self.bits)
    }
}

/// An image coded: the arithmetic-coded part and the raw bits.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Coded {
    pub model: Vec<u8>,
    pub raw: Vec<u8>,
}

/// Contexts of the residual's length, by activity.
const ACT: usize = 48;
/// Lengths of a residual in bits, 0 to 32, as a tree of 6 bits.
const LEN_TREE: usize = 64;
/// Bits after a residual's leading one that are coded with adaptive bits, at most.
const MODELLED: u32 = 1;
/// Contexts of the bias correction.
const BIAS: usize = 9 * 9 * 9;

/// The state of the model, alike on both sides.
struct Model {
    s: Shape,
    /// Trees of the lengths, by activity: `LEN_TREE` bits each (flat, which keeps the
    /// modules from unrolling their initialisation).
    len: Vec<Bit>,
    /// The bits after the leading one ([`MODELLED`] of them at most), by activity (coarse),
    /// length and the bits before.
    second: Vec<Bit>,
    /// Sum of errors and count, per context.
    bias: Vec<(i64, i32)>,
    /// The previous and the current row's residual magnitudes (as lengths).
    elen: [Vec<u8>; 2],
    /// A running estimate of the residuals' magnitude, × 16.
    noise: i64,
}

/// The prediction of a pixel and its contexts.
struct Pred {
    pred: i64,
    act: usize,
    bias: usize,
}

#[inline(always)]
fn len_of(v: u64) -> u32 {
    64 - v.leading_zeros()
}

/// A gradient quantised to −4..=4 against the noise.
#[inline(always)]
fn quant(g: i64, noise: i64) -> usize {
    let a = (g.unsigned_abs() as i64 * 32) / (noise + 16);
    let q = match a {
        0 => 0,
        1..=7 => 1,
        8..=31 => 2,
        32..=127 => 3,
        _ => 4,
    };
    (if g < 0 { 4 - q } else { 4 + q }) as usize
}

impl Model {
    fn new(s: Shape) -> Model {
        Model {
            s,
            len: vec![Bit::default(); LEN_TREE * ACT],
            second: vec![Bit::default(); (12 * 33) << MODELLED],
            bias: vec![(0, 0); BIAS],
            elen: [vec![0; s.width + 2], vec![0; s.width + 2]],
            noise: 16,
        }
    }

    /// The prediction of the pixel at `x` of the row `cur` (pixels before `x` known), the row
    /// above `up` (None for the first).
    #[inline(always)]
    fn predict(&self, cur: &[u32], up: Option<&[u32]>, x: usize) -> Pred {
        let v = |p: u32| self.s.value(p);
        let w = self.s.width;
        let (a, b, c, d) = match up {
            Some(up) => {
                let b = v(up[x]);
                let a = if x > 0 { v(cur[x - 1]) } else { b };
                let c = if x > 0 { v(up[x - 1]) } else { b };
                let d = if x + 1 < w { v(up[x + 1]) } else { b };
                (a, b, c, d)
            }
            None => {
                let a = if x > 0 { v(cur[x - 1]) } else { 0 };
                (a, a, a, a)
            }
        };
        // LOCO-I's median edge detector
        let med = (a + b - c).max(a.min(b)).min(a.max(b));
        let bias = (quant(d - b, self.noise) * 9 + quant(b - c, self.noise)) * 9 + quant(c - a, self.noise);
        let (sum, n) = self.bias[bias];
        let corr = if n > 0 { (sum + (n as i64 >> 1)).div_euclid(n as i64) } else { 0 };
        let pred = med + corr;
        // activity: the gradients, and the lengths of the residuals left, above, above right
        let [prev, now] = &self.elen;
        let g = (a - c).unsigned_abs() + (b - c).unsigned_abs() + (b - d).unsigned_abs();
        let e = (1u64 << now[x]) + (1u64 << prev[x + 1]) + (1u64 << prev[x + 2]) - 3;
        let t = g + 2 * e;
        // half octaves
        let l = len_of(t);
        let act = if l < 2 { l as usize } else { (2 * l - 1 + ((t >> (l - 2)) & 1) as u32) as usize };
        Pred { pred, act: act.min(ACT - 1), bias }
    }

    /// The residual of pixel `p` against `pr`, wrapped to the pixels' bits, zigzagged.
    #[inline(always)]
    fn residual(&self, p: u32, pr: &Pred) -> u32 {
        let e = (p as i64 - pr.pred) as u32 & self.s.mask();
        let bits = self.s.bits;
        let s = ((e << (32 - bits)) as i32) >> (32 - bits);
        ((s << 1) ^ (s >> 31)) as u32 & self.s.mask()
    }

    /// The pixel of zigzagged residual `z` against `pr`.
    #[inline(always)]
    fn pixel(&self, z: u32, pr: &Pred) -> u32 {
        let e = (z >> 1) ^ (z & 1).wrapping_neg();
        (pr.pred as u32).wrapping_add(e) & self.s.mask()
    }

    /// Updates the model with pixel `p` at `x`, of residual `z` against `pr`.
    #[inline(always)]
    fn update(&mut self, p: u32, x: usize, z: u32, pr: &Pred) {
        let err = self.s.value(p) - pr.pred;
        let b = &mut self.bias[pr.bias];
        // (errors as they wrap are kept as they are: they are rare)
        b.0 += err.clamp(-(1 << 40), 1 << 40);
        b.1 += 1;
        if b.1 >= 64 {
            b.0 /= 2;
            b.1 /= 2;
        }
        let mag = (z >> 1) as i64;
        self.noise += (mag.min(1 << 40) * 16 - self.noise) >> 6;
        self.elen[1][x + 1] = len_of(z as u64 >> 1) as u8;
    }

    fn next_row(&mut self) {
        self.elen.swap(0, 1);
    }
}

/// Codes the pixels of `image` but those `known` (all where None).
pub fn encode(image: &[u32], s: Shape, known: Option<&[bool]>) -> Coded {
    let mut m = Model::new(s);
    let mut ac = ArithEncoder::new(Vec::with_capacity(image.len()));
    let mut raw = bits::Writer::with_capacity(image.len());
    for (y, row) in image.chunks(s.width).enumerate() {
        let up = (y > 0).then(|| &image[(y - 1) * s.width..y * s.width]);
        for x in 0..row.len() {
            let pr = m.predict(row, up, x);
            let z = m.residual(row[x], &pr);
            if !known.is_some_and(|k| k[y * s.width + x]) {
                let n = len_of(z as u64);
                let tree = &mut m.len[pr.act * LEN_TREE..][..LEN_TREE];
                let mut node = 1;
                for i in (0..6).rev() {
                    let bit = (n >> i) & 1 != 0;
                    ac.encode(bit, tree[node].p1());
                    tree[node].update(bit);
                    node = node * 2 + bit as usize;
                }
                if n >= 2 {
                    let modelled = (n - 1).min(MODELLED);
                    let base = ((pr.act / 4) * 33 + n as usize) << MODELLED;
                    let mut node = 1;
                    for i in 0..modelled {
                        let bit = (z >> (n - 2 - i)) & 1 != 0;
                        let s2 = &mut m.second[base | node];
                        ac.encode(bit, s2.p1());
                        s2.update(bit);
                        node = node * 2 + bit as usize;
                    }
                    raw.put(z, n - 1 - modelled);
                }
            }
            m.update(row[x], x, z, &pr);
        }
        m.next_row();
    }
    Coded { model: ac.finish(), raw: raw.finish() }
}

/// Decodes into `image` the pixels not `known` (those are there already).
pub fn decode(c: &Coded, s: Shape, image: &mut [u32], known: Option<&[bool]>) -> crate::Result<()> {
    let mut m = Model::new(s);
    let mut ac = ArithDecoder::new(&c.model);
    let mut raw = bits::Reader::new(&c.raw);
    for y in 0..image.len().div_ceil(s.width) {
        let (done, rest) = image.split_at_mut(y * s.width);
        let up = (y > 0).then(|| &done[(y - 1) * s.width..]);
        let n = s.width.min(rest.len());
        let row = &mut rest[..n];
        for x in 0..row.len() {
            let pr = m.predict(row, up, x);
            let z = if !known.is_some_and(|k| k[y * s.width + x]) {
                let tree = &mut m.len[pr.act * LEN_TREE..][..LEN_TREE];
                let mut node = 1;
                for _ in 0..6 {
                    let bit = ac.decode(tree[node].p1());
                    tree[node].update(bit);
                    node = node * 2 + bit as usize;
                }
                let n = (node - LEN_TREE) as u32;
                if n > s.bits {
                    return Err(crate::corrupt());
                }
                let z = match n {
                    0 => 0,
                    1 => 1,
                    _ => {
                        let modelled = (n - 1).min(MODELLED);
                        let base = ((pr.act / 4) * 33 + n as usize) << MODELLED;
                        let mut node = 1usize;
                        for _ in 0..modelled {
                            let s2 = &mut m.second[base | node];
                            let bit = ac.decode(s2.p1());
                            s2.update(bit);
                            node = node * 2 + bit as usize;
                        }
                        let rest = n - 1 - modelled;
                        (node as u32) << rest | raw.get(rest)
                    }
                };
                row[x] = m.pixel(z, &pr);
                z
            } else {
                m.residual(row[x], &pr)
            };
            m.update(row[x], x, z, &pr);
        }
        m.next_row();
    }
    if ac.overrun() || raw.past_end() {
        return Err(crate::corrupt());
    }
    Ok(())
}

/// How an image is coded: by prediction (this module's model) or as a pyramid
/// ([`crate::pyramid`]), which takes whole rows and no known pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Predict = 0,
    Pyramid = 1,
}

impl Mode {
    pub fn from_byte(b: u8) -> Option<Mode> {
        match b {
            0 => Some(Mode::Predict),
            1 => Some(Mode::Pyramid),
            _ => None,
        }
    }
}

/// Codes the pixels of `image` but those `known` in `mode`; None where the mode does not take
/// the image.
pub fn encode_as(image: &[u32], s: Shape, known: Option<&[bool]>, mode: Mode) -> Option<Coded> {
    match mode {
        Mode::Predict => Some(encode(image, s, known)),
        Mode::Pyramid => {
            if known.is_some() || !image.len().is_multiple_of(s.width) {
                return None;
            }
            let v: Vec<i64> = image.iter().map(|&x| s.value(x)).collect();
            Some(crate::pyramid::encode(&v, s.width, image.len() / s.width))
        }
    }
}

/// Decodes into `image` the pixels of [`encode_as`] in `mode`.
pub fn decode_as(c: &Coded, s: Shape, image: &mut [u32], known: Option<&[bool]>, mode: Mode) -> crate::Result<()> {
    match mode {
        Mode::Predict => decode(c, s, image, known),
        Mode::Pyramid => {
            if known.is_some() || !image.len().is_multiple_of(s.width) {
                return Err(crate::corrupt());
            }
            let v = crate::pyramid::decode(c, s.width, image.len() / s.width)?;
            for (p, v) in image.iter_mut().zip(v) {
                *p = v as u32 & s.mask();
            }
            Ok(())
        }
    }
}

/// Pixels of the sample [`encode_best`] tries the modes on.
const SAMPLE: usize = 1 << 20;

/// The mode that codes `image` the smaller, tried on its first rows (about [`SAMPLE`]
/// pixels), and the image coded so.
pub fn encode_best(image: &[u32], s: Shape, known: Option<&[bool]>) -> (Mode, Coded) {
    let size = |c: &Coded| c.model.len() + c.raw.len();
    let rows = (SAMPLE / s.width).max(4) * s.width;
    if rows < image.len() {
        let sample = &image[..rows];
        let k = known.map(|k| &k[..rows]);
        let predict = encode(sample, s, k);
        let mode = match encode_as(sample, s, k, Mode::Pyramid) {
            Some(p) if size(&p) < size(&predict) => Mode::Pyramid,
            _ => Mode::Predict,
        };
        return (mode, encode_as(image, s, known, mode).unwrap());
    }
    let predict = encode(image, s, known);
    match encode_as(image, s, known, Mode::Pyramid) {
        Some(p) if size(&p) < size(&predict) => (Mode::Pyramid, p),
        _ => (Mode::Predict, predict),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aqr_common::rng::Rng;

    #[test]
    fn round_trip() {
        let mut rng = Rng::new(7);
        for (bits, signed) in [(8, false), (16, true), (16, false), (32, true)] {
            let s = Shape { width: 37, bits, signed };
            for spread in [1u64, 30, 1 << 12, 1 << 33] {
                let image: Vec<u32> =
                    (0..37 * 23 + 5).map(|i| ((i as u64 * 3 + rng.below(spread)) as u32) & s.mask()).collect();
                let known: Vec<bool> = (0..image.len()).map(|i| i % 7 == 3).collect();
                for k in [None, Some(&known[..])] {
                    let c = encode(&image, s, k);
                    let mut back: Vec<u32> =
                        image.iter().zip(&known).map(|(&p, &kn)| if k.is_some() && kn { p } else { 0 }).collect();
                    decode(&c, s, &mut back, k).unwrap();
                    assert_eq!(back, image);
                }
            }
        }
    }
}
