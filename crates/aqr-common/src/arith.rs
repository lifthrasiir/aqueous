//! Carry-less binary arithmetic coding (32-bit), and adaptive bits: the records of aqr-lzma,
//! aqr-lzo, aqr-bvx and aqr-gif.

use std::hint::select_unpredictable;

pub struct ArithEncoder {
    x1: u32,
    x2: u32,
    out: Vec<u8>,
}

impl ArithEncoder {
    pub fn new(out: Vec<u8>) -> Self {
        ArithEncoder { x1: 0, x2: u32::MAX, out }
    }

    /// Codes `bit` of probability `p1`/65536 of being 1.
    #[inline]
    pub fn encode(&mut self, bit: bool, p1: u32) {
        let xmid = self.x1 + (((self.x2 - self.x1) as u64 * p1 as u64) >> 16) as u32;
        if bit {
            self.x2 = xmid;
        } else {
            self.x1 = xmid + 1;
        }
        while (self.x1 ^ self.x2) & 0xff00_0000 == 0 {
            self.out.push((self.x2 >> 24) as u8);
            self.x1 <<= 8;
            self.x2 = self.x2 << 8 | 0xff;
        }
    }

    /// As [`ArithEncoder::encode`], by selects rather than a branch on `bit`: for bits hard to
    /// predict (as aqr-tex's indices), where a mispredicted branch costs more than the
    /// selects' wait for the bit. (The coding is the same.)
    #[inline]
    pub fn encode_unpredictable(&mut self, bit: bool, p1: u32) {
        let xmid = self.x1 + (((self.x2 - self.x1) as u64 * p1 as u64) >> 16) as u32;
        self.x2 = select_unpredictable(bit, xmid, self.x2);
        self.x1 = select_unpredictable(bit, self.x1, xmid + 1);
        while (self.x1 ^ self.x2) & 0xff00_0000 == 0 {
            self.out.push((self.x2 >> 24) as u8);
            self.x1 <<= 8;
            self.x2 = self.x2 << 8 | 0xff;
        }
    }

    /// Any value in (x1, x2] decodes, and the decoder reads zeros past the end.
    pub fn finish(mut self) -> Vec<u8> {
        self.out.push((self.x1 >> 24) as u8 + 1);
        self.out
    }
}

pub struct ArithDecoder<'a> {
    x1: u32,
    x2: u32,
    x: u32,
    input: &'a [u8],
    pos: usize,
}

impl<'a> ArithDecoder<'a> {
    pub fn new(input: &'a [u8]) -> Self {
        let mut d = ArithDecoder { x1: 0, x2: u32::MAX, x: 0, input, pos: 0 };
        for _ in 0..4 {
            d.x = d.x << 8 | d.byte() as u32;
        }
        d
    }

    #[inline]
    fn byte(&mut self) -> u8 {
        let b = self.input.get(self.pos).copied().unwrap_or(0);
        self.pos += 1;
        b
    }

    #[inline]
    pub fn decode(&mut self, p1: u32) -> bool {
        let xmid = self.x1 + (((self.x2 - self.x1) as u64 * p1 as u64) >> 16) as u32;
        let bit = self.x <= xmid;
        if bit {
            self.x2 = xmid;
        } else {
            self.x1 = xmid + 1;
        }
        while (self.x1 ^ self.x2) & 0xff00_0000 == 0 {
            self.x1 <<= 8;
            self.x2 = self.x2 << 8 | 0xff;
            self.x = self.x << 8 | self.byte() as u32;
        }
        bit
    }

    /// As [`ArithDecoder::decode`], by selects (as [`ArithEncoder::encode_unpredictable`]).
    #[inline]
    pub fn decode_unpredictable(&mut self, p1: u32) -> bool {
        let xmid = self.x1 + (((self.x2 - self.x1) as u64 * p1 as u64) >> 16) as u32;
        let bit = self.x <= xmid;
        self.x2 = select_unpredictable(bit, xmid, self.x2);
        self.x1 = select_unpredictable(bit, self.x1, xmid + 1);
        while (self.x1 ^ self.x2) & 0xff00_0000 == 0 {
            self.x1 <<= 8;
            self.x2 = self.x2 << 8 | 0xff;
            self.x = self.x << 8 | self.byte() as u32;
        }
        bit
    }

    pub fn overrun(&self) -> bool {
        self.pos > self.input.len() + 4
    }
}

/// An adaptive bit: the probability of a 1, learnt at 1/(n + 1.5) for its first uses.
#[derive(Clone, Copy)]
pub struct Bit {
    p: u16,
    n: u8,
}

const LIMIT: usize = 30;
const RATE: [i32; LIMIT + 1] = {
    let mut r = [0; LIMIT + 1];
    let mut i = 0;
    while i <= LIMIT {
        r[i] = 65536 * 2 / (2 * i as i32 + 3);
        i += 1;
    }
    r
};

impl Default for Bit {
    fn default() -> Self {
        Bit { p: 1 << 15, n: 0 }
    }
}

impl Bit {
    #[inline]
    pub fn p1(self) -> u32 {
        self.p as u32
    }

    #[inline]
    pub fn update(&mut self, bit: bool) {
        let t = if bit { 65535 } else { 0 };
        let p = self.p as i32 + (((t - self.p as i32) * RATE[self.n as usize]) >> 16);
        self.p = p.clamp(32, 65536 - 32) as u16;
        if (self.n as usize) < LIMIT {
            self.n += 1;
        }
    }
}

/// A side of the coder of a record: the encoder (which writes the value given and returns it)
/// or the decoder (which returns what it reads), so that one walk over the record does both.
/// The bits, `bit`'s under the adaptive bit given and `raw`'s of probability 1/2, are the
/// same either way; what is built of them here ([`Coder::gamma32`], [`Coder::raws`],
/// [`Coder::tree`]) is too.
pub trait Coder {
    fn bit(&mut self, b: &mut Bit, v: bool) -> bool;
    fn raw(&mut self, v: bool) -> bool;
    /// Whether the decoder has read past its input.
    fn failed(&self) -> bool;
    /// The kind of what the bits after are, for the statistics ([`Costed`]).
    fn set_cat(&mut self, _: usize) {}

    /// `v` as `x = v + 1` (wrapping): the bit length of `x` less 1 in unary (adaptive bits of
    /// `t`, the last one for the lengths beyond), then the bits of `x` below its top one, raw.
    /// Decoding, `corrupt()` where the unary runs past 32 bits or the input.
    // (called from many places in a walk: out of line, as each was in its crate)
    #[inline(never)]
    fn gamma32<E>(&mut self, t: &mut [Bit], v: u32, corrupt: impl Fn() -> E) -> Result<u32, E> {
        let x = v.wrapping_add(1);
        let n = 31 - x.max(1).leading_zeros();
        let mut k = 0;
        while self.bit(&mut t[(k as usize).min(t.len() - 1)], k < n) {
            k += 1;
            if k > 31 || self.failed() {
                return Err(corrupt());
            }
        }
        let mut y = 1u32;
        for i in (0..k).rev() {
            y = y << 1 | self.raw(x >> i & 1 != 0) as u32;
        }
        Ok(y - 1)
    }

    /// [`Coder::gamma32`] of 64 bits.
    // (called from many places in a walk: out of line, as each was in its crate)
    #[inline(never)]
    fn gamma64<E>(&mut self, t: &mut [Bit], v: u64, corrupt: impl Fn() -> E) -> Result<u64, E> {
        let x = v.wrapping_add(1);
        let n = 63 - x.max(1).leading_zeros();
        let mut k = 0;
        while self.bit(&mut t[(k as usize).min(t.len() - 1)], k < n) {
            k += 1;
            if k > 63 || self.failed() {
                return Err(corrupt());
            }
        }
        let mut y = 1u64;
        for i in (0..k).rev() {
            y = y << 1 | self.raw(x >> i & 1 != 0) as u64;
        }
        Ok(y - 1)
    }

    /// `n` raw bits of `v`, the high one first.
    fn raws(&mut self, n: u32, v: u64) -> u64 {
        (0..n).rev().fold(0, |x, i| x << 1 | self.raw(v >> i & 1 != 0) as u64)
    }

    /// `v` of `n` bits, by a binary tree of adaptive bits `t` (of `1 << n` bits).
    fn tree(&mut self, t: &mut [Bit], n: u32, v: u32) -> u32 {
        let mut k = 1usize;
        for i in (0..n).rev() {
            let b = self.bit(&mut t[k], v >> i & 1 != 0);
            k = k << 1 | b as usize;
        }
        (k - (1 << n)) as u32
    }
}

impl Coder for ArithEncoder {
    #[inline]
    fn bit(&mut self, b: &mut Bit, v: bool) -> bool {
        self.encode(v, b.p1());
        b.update(v);
        v
    }
    #[inline]
    fn raw(&mut self, v: bool) -> bool {
        self.encode(v, 1 << 15);
        v
    }
    fn failed(&self) -> bool {
        false
    }
}

impl Coder for ArithDecoder<'_> {
    #[inline]
    fn bit(&mut self, b: &mut Bit, _: bool) -> bool {
        let v = self.decode(b.p1());
        b.update(v);
        v
    }
    #[inline]
    fn raw(&mut self, _: bool) -> bool {
        self.decode(1 << 15)
    }
    fn failed(&self) -> bool {
        self.overrun()
    }
}

/// The bits a bit of probability `(i + 0.5) / 4096` costs, by `i`: what [`Costed`] counts
/// (a `log2` a bit took 11% of aqr-lzx's splitting, for statistics alone).
static COST: std::sync::LazyLock<[f64; 4096]> =
    std::sync::LazyLock::new(|| std::array::from_fn(|i| -((i as f64 + 0.5) / 4096.0).log2()));

/// The encoder, counting the bits it writes of each of `CATS` kinds ([`Coder::set_cat`]), about.
pub struct Costed<const CATS: usize> {
    pub c: ArithEncoder,
    cat: usize,
    pub cost: [f64; CATS],
}

impl<const CATS: usize> Costed<CATS> {
    /// The encoder, its bits of kind `cat` first.
    pub fn new(cat: usize) -> Self {
        Costed { c: ArithEncoder::new(Vec::new()), cat, cost: [0.0; CATS] }
    }

    /// The encoder `c` (its output so far kept), counting from kind 0.
    pub fn with(c: ArithEncoder) -> Self {
        Costed { c, cat: 0, cost: [0.0; CATS] }
    }
}

impl<const CATS: usize> Coder for Costed<CATS> {
    #[inline]
    fn bit(&mut self, b: &mut Bit, v: bool) -> bool {
        let p1 = b.p1();
        self.cost[self.cat] += COST[((if v { p1 } else { 65536 - p1 }) >> 4).min(4095) as usize];
        self.c.bit(b, v)
    }
    #[inline]
    fn raw(&mut self, v: bool) -> bool {
        self.cost[self.cat] += 1.0;
        self.c.raw(v)
    }
    fn failed(&self) -> bool {
        false
    }
    fn set_cat(&mut self, cat: usize) {
        self.cat = cat;
    }
}

#[cfg(test)]
mod coder_tests {
    use super::*;

    /// Gammas, raw bits and trees written and read back alike, the bits counted by kind.
    #[test]
    fn coded_both_ways() {
        let vals32 = [0u32, 1, 2, 5, 1000, u32::MAX - 1];
        let vals64 = [0u64, 7, 1 << 40, u64::MAX - 1];
        let mut e = Costed::<2>::new(0);
        let (mut t, mut tree) = (vec![Bit::default(); 8], vec![Bit::default(); 16]);
        for &v in &vals32 {
            assert_eq!(e.gamma32(&mut t, v, || ()), Ok(v));
        }
        e.set_cat(1);
        for &v in &vals64 {
            assert_eq!(e.gamma64(&mut t, v, || ()), Ok(v));
        }
        assert_eq!(e.raws(12, 0xabc), 0xabc);
        assert_eq!(e.tree(&mut tree, 4, 9), 9);
        assert!(e.cost[0] > 0.0 && e.cost[1] > 0.0);
        let data = e.c.finish();
        let mut d = ArithDecoder::new(&data);
        let (mut t, mut tree) = (vec![Bit::default(); 8], vec![Bit::default(); 16]);
        for &v in &vals32 {
            assert_eq!(d.gamma32(&mut t, 0, || ()), Ok(v));
        }
        for &v in &vals64 {
            assert_eq!(d.gamma64(&mut t, 0, || ()), Ok(v));
        }
        assert_eq!(d.raws(12, 0), 0xabc);
        assert_eq!(d.tree(&mut tree, 4, 0), 9);
        assert!(!d.failed());
    }
}
