//! The float model (FORMAT.md, §4): IEEE floats (binary32 or binary64) predicted by a linear
//! predictor of the samples before, its coefficients given for each block, and the difference
//! of the sample from the prediction taken in the order of floats as integers: its length in
//! bits coded in the context of the lengths before, the two bits below its leading one too,
//! the others as they are. The low bits that are zero in every sample (of binary32 values in
//! binary64, say) are left out.

use aqr_common::arith::{ArithDecoder, ArithEncoder, Bit};

/// Samples of a block, which has coefficients of its own.
pub const BLOCK: usize = 1 << 16;
pub const MAX_ORDER: usize = 32;

/// Contexts of a length (the lengths before), and the lengths (0 to 64).
const CONTEXTS: usize = 65;
/// Bits below the leading one that are coded.
const TOP: u32 = 2;

/// Samples of binary64 (`wide`) or binary32, without their `shift` low bits.
#[derive(Clone, Copy)]
struct Kind {
    wide: bool,
    shift: u32,
}

impl Kind {
    /// Bits of a sample as coded.
    fn width(self) -> u32 {
        (if self.wide { 64u32 } else { 32 }).saturating_sub(self.shift).max(1)
    }

    fn mask(self) -> u64 {
        u64::MAX >> (64 - self.width())
    }

    /// The bits of a sample as an integer of the floats' order: a negative one's bits all
    /// flipped, a positive one's sign set (its own inverse is [`Kind::unordered`]).
    #[inline]
    fn ordered(self, bits: u64) -> u64 {
        let sign = 1u64 << (self.width() - 1);
        if bits & sign != 0 { !bits & self.mask() } else { bits | sign }
    }

    #[inline]
    fn unordered(self, o: u64) -> u64 {
        let sign = 1u64 << (self.width() - 1);
        if o & sign != 0 { o & !sign } else { !o & self.mask() }
    }

    /// A sample's value from its bits.
    #[inline]
    fn value(self, bits: u64) -> f64 {
        let b = bits << self.shift;
        if self.wide { f64::from_bits(b) } else { f32::from_bits(b as u32) as f64 }
    }

    /// The prediction of sample `i` of `x` (the values, those before `i` known) by `w`, in the
    /// order of floats (its low bits cut off); where it is not finite, that of the sample
    /// before (or of +0).
    #[inline]
    fn predict(self, x: &[f64], o: &[u64], i: usize, w: &[f64]) -> u64 {
        let mut p = 0.0f64;
        for (k, &c) in w.iter().enumerate() {
            if let Some(j) = i.checked_sub(k + 1) {
                p += c * x[j];
            }
        }
        let bits = if self.wide {
            Some(p).filter(|p| p.is_finite()).map(f64::to_bits)
        } else {
            Some(p as f32).filter(|p| p.is_finite()).map(|p| p.to_bits() as u64)
        };
        match bits {
            Some(b) => self.ordered(b >> self.shift),
            None => i.checked_sub(1).map_or(self.ordered(0), |j| o[j]),
        }
    }
}

/// The adaptive models.
struct Model {
    /// The length's bits, a binary tree of 128 leaves (of which 65 used) by context.
    lengths: Vec<[Bit; 128]>,
    /// The bits below the leading one, a tree of 3 nodes by length.
    top: [[Bit; 4]; 65],
}

impl Model {
    fn new() -> Model {
        Model { lengths: vec![[Bit::default(); 128]; CONTEXTS], top: [[Bit::default(); 4]; 65] }
    }
}

/// The context of the next length from the last three.
#[inline]
fn context(l: [u32; 3]) -> usize {
    ((2 * l[0] + l[1] + l[2] + 2) / 4) as usize
}

/// A float model's coded parts.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Coded {
    /// The coefficients of each block, binary64 little-endian.
    pub coefs: Vec<u8>,
    /// The arithmetic code of the lengths and the bits below the leading ones.
    pub model: Vec<u8>,
    /// The other bits, MSB first.
    pub raw: Vec<u8>,
}

/// Bits written MSB first.
#[derive(Default)]
struct Bits {
    out: Vec<u8>,
    acc: u64,
    n: u32,
}

impl Bits {
    /// The low `n` bits of `v`, `n` at most 64.
    #[inline]
    fn put(&mut self, v: u64, n: u32) {
        if n > 32 {
            self.put(v >> 32, n - 32);
            self.put(v & 0xffff_ffff, 32);
            return;
        }
        self.acc = self.acc << n | (v & ((1u64 << n) - 1));
        self.n += n;
        while self.n >= 8 {
            self.n -= 8;
            self.out.push((self.acc >> self.n) as u8);
        }
    }

    fn finish(mut self) -> Vec<u8> {
        if self.n > 0 {
            self.out.push((self.acc << (8 - self.n)) as u8);
        }
        self.out
    }
}

/// Bits read MSB first, zeros past the end.
struct Reader<'a> {
    d: &'a [u8],
    pos: usize,
}

impl Reader<'_> {
    #[inline]
    fn get(&mut self, n: u32) -> u64 {
        if n > 32 {
            let hi = self.get(n - 32);
            return hi << 32 | self.get(32);
        }
        if n == 0 {
            return 0;
        }
        let at = self.pos >> 3;
        let w = match self.d.get(at..at + 8) {
            Some(b) => u64::from_be_bytes(b.try_into().unwrap()),
            None => {
                let mut b = [0u8; 8];
                let tail = self.d.get(at..).unwrap_or(&[]);
                b[..tail.len()].copy_from_slice(tail);
                u64::from_be_bytes(b)
            }
        };
        self.pos += n as usize;
        (w << (self.pos - n as usize & 7)) >> (64 - n)
    }
}

/// Bits of the zigzag of a difference of `width` bits.
#[inline]
fn zigzag(d: u64, width: u32) -> u64 {
    let mask = if width == 64 { u64::MAX } else { (1u64 << width) - 1 };
    let d = d & mask;
    let neg = d >> (width - 1) & 1;
    ((d << 1) ^ neg.wrapping_neg()) & mask
}

#[inline]
fn unzigzag(z: u64, width: u32) -> u64 {
    let mask = if width == 64 { u64::MAX } else { (1u64 << width) - 1 };
    ((z >> 1) ^ (z & 1).wrapping_neg()) & mask
}

/// The coded parts of samples `bits` (of binary64 where `wide`, else binary32, their `shift`
/// low bits zero) with coefficients of `order` ([`coefficients`] of each block).
pub fn encode(bits: &[u64], wide: bool, shift: u32, order: usize) -> Coded {
    let k = Kind { wide, shift };
    let x: Vec<f64> = bits.iter().map(|&b| k.value(b >> shift)).collect();
    let o: Vec<u64> = bits.iter().map(|&b| k.ordered(b >> shift)).collect();
    let width = k.width();
    let mut m = Model::new();
    let mut e = ArithEncoder::new(Vec::new());
    let mut raw = Bits::default();
    let mut coefs = Vec::new();
    let mut last = [0u32; 3];
    for start in (0..x.len()).step_by(BLOCK) {
        let end = (start + BLOCK).min(x.len());
        let w = coefficients(&x, start, end, order);
        for c in &w {
            coefs.extend_from_slice(&c.to_le_bytes());
        }
        for i in start..end {
            let z = zigzag(o[i].wrapping_sub(k.predict(&x, &o, i, &w)), width);
            let l = 64 - z.leading_zeros();
            let tree = &mut m.lengths[context(last)];
            let mut node = 1;
            for b in (0..7).rev() {
                let bit = l >> b & 1 != 0;
                e.encode(bit, tree[node].p1());
                tree[node].update(bit);
                node = node << 1 | bit as usize;
            }
            last = [l, last[0], last[1]];
            if l >= 2 {
                let top = TOP.min(l - 1);
                let t = &mut m.top[l as usize];
                let mut node = 1;
                for b in 0..top {
                    let bit = z >> (l - 2 - b) & 1 != 0;
                    e.encode(bit, t[node].p1());
                    t[node].update(bit);
                    node = node << 1 | bit as usize;
                }
                raw.put(z, l - 1 - top);
            }
        }
    }
    Coded { coefs, model: e.finish(), raw: raw.finish() }
}

/// The bits of `n` samples (binary64 where `wide`, else binary32, of `shift` low bits zero) of
/// `order` coefficients from their coded parts ([`Coded`]), or None where the coefficients are
/// not as many as the blocks need.
#[allow(clippy::too_many_arguments)]
pub fn decode(
    coefs: &[u8],
    model: &[u8],
    raw: &[u8],
    n: usize,
    wide: bool,
    shift: u32,
    order: usize,
) -> Option<Vec<u64>> {
    if order > MAX_ORDER || coefs.len() != n.div_ceil(BLOCK) * order * 8 || shift >= if wide { 64 } else { 32 } {
        return None;
    }
    let k = Kind { wide, shift };
    let width = k.width();
    let mut x = vec![0f64; n];
    let mut o = vec![0u64; n];
    let mut m = Model::new();
    let mut d = ArithDecoder::new(model);
    let mut bits = Reader { d: raw, pos: 0 };
    let mut last = [0u32; 3];
    let mut w = vec![0f64; order];
    for (b, start) in (0..n).step_by(BLOCK).enumerate() {
        let end = (start + BLOCK).min(n);
        for (j, c) in coefs[b * order * 8..][..order * 8].chunks_exact(8).enumerate() {
            w[j] = f64::from_le_bytes(c.try_into().unwrap());
        }
        for i in start..end {
            let tree = &mut m.lengths[context(last)];
            let mut node = 1;
            for _ in 0..7 {
                let bit = d.decode(tree[node].p1());
                tree[node].update(bit);
                node = node << 1 | bit as usize;
            }
            let l = (node - 128) as u32;
            if l > width {
                return None;
            }
            last = [l, last[0], last[1]];
            let z = match l {
                0 => 0,
                1 => 1,
                _ => {
                    let top = TOP.min(l - 1);
                    let t = &mut m.top[l as usize];
                    let mut node = 1;
                    let mut v = 1u64;
                    for _ in 0..top {
                        let bit = d.decode(t[node].p1());
                        t[node].update(bit);
                        node = node << 1 | bit as usize;
                        v = v << 1 | bit as u64;
                    }
                    let rest = l - 1 - top;
                    v << rest | bits.get(rest)
                }
            };
            o[i] = k.predict(&x, &o, i, &w).wrapping_add(unzigzag(z, width)) & k.mask();
            x[i] = k.value(k.unordered(o[i]));
        }
    }
    if d.overrun() || bits.pos > raw.len() * 8 {
        return None;
    }
    Some(o.into_iter().map(|o| k.unordered(o) << shift).collect())
}

/// The least-squares coefficients of order `order` predicting samples `start..end` of `x`
/// from those before (the covariance method; samples that are not finite taken as 0, the
/// values scaled by a power of two, which leaves the coefficients as they are).
pub fn coefficients(x: &[f64], start: usize, end: usize, order: usize) -> Vec<f64> {
    if order == 0 {
        return Vec::new();
    }
    let from = start.saturating_sub(order + 1);
    let max = x[from..end].iter().filter(|v| v.is_finite()).fold(0f64, |a, v| a.max(v.abs()));
    if max == 0.0 {
        return vec![0.0; order];
    }
    let scale = 2f64.powi(-(max.log2().ceil() as i32));
    let at = |j: isize| -> f64 {
        if j < 0 {
            return 0.0;
        }
        let v = x[j as usize];
        if v.is_finite() { v * scale } else { 0.0 }
    };
    let (s, e) = (start as isize, end as isize);
    // r[j][k] = Σ x[i−1−j] x[i−1−k], b[k] = Σ x[i] x[i−1−k], for i in start..end
    let mut r = vec![vec![0f64; order]; order];
    let mut b = vec![0f64; order];
    for i in s..e {
        let xi = at(i);
        let x1 = at(i - 1);
        for k in 0..order {
            let xk = at(i - 1 - k as isize);
            r[0][k] += x1 * xk;
            b[k] += xi * xk;
        }
    }
    for j in 1..order {
        for k in j..order {
            let (j1, k1) = (j as isize, k as isize);
            r[j][k] = r[j - 1][k - 1] + at(s - 1 - j1) * at(s - 1 - k1) - at(e - 1 - j1) * at(e - 1 - k1);
        }
    }
    for j in 0..order {
        for k in 0..j {
            r[j][k] = r[k][j];
        }
    }
    // a little ridge, so that the system is definite
    let trace: f64 = (0..order).map(|k| r[k][k]).sum();
    let ridge = trace / order as f64 * 1e-9 + f64::MIN_POSITIVE;
    for (k, row) in r.iter_mut().enumerate() {
        row[k] += ridge;
    }
    cholesky_solve(r, b).unwrap_or_else(|| vec![0.0; order])
}

/// The solution of `a x = b` for a symmetric positive definite `a`.
fn cholesky_solve(mut a: Vec<Vec<f64>>, mut b: Vec<f64>) -> Option<Vec<f64>> {
    let n = b.len();
    for j in 0..n {
        let mut d = a[j][j];
        for k in 0..j {
            d -= a[j][k] * a[j][k];
        }
        if !(d > 0.0) {
            return None;
        }
        let d = d.sqrt();
        a[j][j] = d;
        for i in j + 1..n {
            let mut s = a[i][j];
            for k in 0..j {
                s -= a[i][k] * a[j][k];
            }
            a[i][j] = s / d;
        }
    }
    for i in 0..n {
        let mut s = b[i];
        for k in 0..i {
            s -= a[i][k] * b[k];
        }
        b[i] = s / a[i][i];
    }
    for i in (0..n).rev() {
        let mut s = b[i];
        for k in i + 1..n {
            s -= a[k][i] * b[k];
        }
        b[i] = s / a[i][i];
    }
    b.iter().all(|v| v.is_finite()).then_some(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(bits: &[u64], wide: bool, order: usize) -> usize {
        let or = bits.iter().fold(0, |a, b| a | b);
        let shift = or.trailing_zeros().min(if wide { 63 } else { 31 });
        let c = encode(bits, wide, shift, order);
        assert_eq!(decode(&c.coefs, &c.model, &c.raw, bits.len(), wide, shift, order).as_deref(), Some(bits));
        c.coefs.len() + c.model.len() + c.raw.len()
    }

    #[test]
    fn floats() {
        let mut s = 1u64;
        let mut noise = || {
            s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (s >> 11) as f64 / (1u64 << 53) as f64 - 0.5
        };
        let wave: Vec<f64> = (0..200_000).map(|i| (i as f64 / 30.0).sin() * 1e-19 + noise() * 1e-22).collect();
        let bits: Vec<u64> = wave.iter().map(|v| v.to_bits()).collect();
        let n = roundtrip(&bits, true, 32);
        assert!(n < bits.len() * 8 * 9 / 10, "{n}");
        let bits32: Vec<u64> = wave.iter().map(|&v| (v as f32).to_bits() as u64).collect();
        roundtrip(&bits32, false, 16);
        // binary32 values in binary64: their low 29 bits left out
        let narrow: Vec<u64> = wave.iter().map(|&v| (v as f32 as f64).to_bits()).collect();
        assert!(roundtrip(&narrow, true, 32) < roundtrip(&bits32, false, 32) + 1000);
        // not finite, zeros, extremes; no coefficients
        let odd = [f64::NAN, 0.0, -0.0, f64::INFINITY, f64::MAX, -f64::MAX, 1e-310, f64::NEG_INFINITY, 1.0, 1.0];
        let bits: Vec<u64> = odd.iter().cycle().take(5000).map(|v| v.to_bits()).collect();
        roundtrip(&bits, true, 8);
        roundtrip(&bits, true, 0);
        let bits32: Vec<u64> = odd.iter().cycle().take(5000).map(|&v| (v as f32).to_bits() as u64).collect();
        roundtrip(&bits32, false, 4);
        roundtrip(&[], true, 32);
    }
}
