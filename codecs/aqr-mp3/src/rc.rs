//! Adaptive binary range coder (LZMA style) for the side information. One code path serves
//! both directions: `c.bit(p, b)` codes `b` when compressing and returns the decoded bit
//! when decompressing.

const PROB_BITS: u32 = 11;
const ONE: u16 = 1 << PROB_BITS;
const MOVE: u32 = 5;
const TOP: u32 = 1 << 24;

/// Probability of a 0 bit, in 1/2048.
pub type Prob = u16;
pub const P_INIT: Prob = ONE / 2;

pub trait BitCoder {
    fn bit(&mut self, p: &mut Prob, b: u32) -> u32;

    /// `n`-bit value (MSB first) through a binary tree of `2^n` probabilities.
    #[inline]
    fn tree(&mut self, probs: &mut [Prob], n: u32, v: u32) -> u32 {
        debug_assert!(probs.len() >= 1 << n);
        let mut m = 1usize;
        for i in (0..n).rev() {
            let b = self.bit(&mut probs[m], v >> i & 1);
            m = m << 1 | b as usize;
        }
        (m - (1 << n)) as u32
    }

    /// Unsigned value: Elias-gamma style, the bit length through `p.len`, then the bits below
    /// the top one through `p.bits` (by length and position).
    #[inline]
    fn uint(&mut self, p: &mut UIntProbs, v: u32) -> u32 {
        // Length: unary with a probability per position.
        let l = 32 - v.leading_zeros();
        let mut n = 0;
        while n < 32 && self.bit(&mut p.len[n as usize], (n < l) as u32) != 0 {
            n += 1;
        }
        if n <= 1 {
            return n;
        }
        let mut r = 1u32;
        for i in (0..n - 1).rev() {
            let k = (n - 1 - i).min(2) as usize;
            let b = self.bit(&mut p.bits[n as usize][k], v >> i & 1);
            r = r << 1 | b;
        }
        r
    }

    /// Signed value: zero flag, sign, magnitude - 1.
    #[inline]
    fn sint(&mut self, p: &mut SIntProbs, v: i32) -> i32 {
        if self.bit(&mut p.zero, (v != 0) as u32) == 0 {
            return 0;
        }
        let neg = self.bit(&mut p.sign, (v < 0) as u32);
        let m = self.uint(&mut p.mag, v.unsigned_abs().wrapping_sub(1)) + 1;
        if neg != 0 { -(m as i32) } else { m as i32 }
    }
}

#[derive(Clone)]
pub struct UIntProbs {
    len: [Prob; 33],
    /// By bit length, then by position (first, second, rest) below the top bit.
    bits: [[Prob; 3]; 33],
}

impl Default for UIntProbs {
    fn default() -> Self {
        UIntProbs { len: [P_INIT; 33], bits: [[P_INIT; 3]; 33] }
    }
}

#[derive(Clone)]
pub struct SIntProbs {
    zero: Prob,
    sign: Prob,
    mag: UIntProbs,
}

impl Default for SIntProbs {
    fn default() -> Self {
        SIntProbs { zero: P_INIT, sign: P_INIT, mag: UIntProbs::default() }
    }
}

pub struct Encoder {
    low: u64,
    range: u32,
    cache: u8,
    cache_size: u64,
    pub out: Vec<u8>,
}

impl Default for Encoder {
    fn default() -> Self {
        Encoder { low: 0, range: u32::MAX, cache: 0, cache_size: 1, out: Vec::new() }
    }
}

impl Encoder {
    fn shift_low(&mut self) {
        if (self.low as u32) < 0xff00_0000 || self.low >= 1 << 32 {
            let carry = (self.low >> 32) as u8;
            let mut c = self.cache;
            loop {
                self.out.push(c.wrapping_add(carry));
                c = 0xff;
                self.cache_size -= 1;
                if self.cache_size == 0 {
                    break;
                }
            }
            self.cache = (self.low >> 24) as u8;
        }
        self.cache_size += 1;
        self.low = (self.low & 0x00ff_ffff) << 8;
    }

    pub fn finish(mut self) -> Vec<u8> {
        for _ in 0..5 {
            self.shift_low();
        }
        // The first byte is always 0 (the initial cache).
        self.out.remove(0);
        self.out
    }
}

impl BitCoder for Encoder {
    #[inline]
    fn bit(&mut self, p: &mut Prob, b: u32) -> u32 {
        let bound = (self.range >> PROB_BITS) * *p as u32;
        let one = b != 0;
        self.low += if one { bound as u64 } else { 0 };
        self.range = std::hint::select_unpredictable(one, self.range - bound, bound);
        // p += (ONE - p) >> MOVE for 0, p -= p >> MOVE for 1.
        let up = (ONE - *p) >> MOVE;
        let down = *p >> MOVE;
        *p = std::hint::select_unpredictable(one, *p - down, *p + up);
        while self.range < TOP {
            self.range <<= 8;
            self.shift_low();
        }
        b
    }
}

pub struct Decoder<'a> {
    code: u32,
    range: u32,
    data: &'a [u8],
    pos: usize,
}

impl<'a> Decoder<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        let mut d = Decoder { code: 0, range: u32::MAX, data, pos: 0 };
        for _ in 0..4 {
            d.code = d.code << 8 | d.next() as u32;
        }
        d
    }

    #[inline]
    fn next(&mut self) -> u8 {
        let b = self.data.get(self.pos).copied().unwrap_or(0);
        self.pos += 1;
        b
    }

    /// True if the decoder has not read far past the data (the encoder flushes 4 bytes).
    pub fn in_bounds(&self) -> bool {
        self.pos <= self.data.len() + 4
    }
}

impl BitCoder for Decoder<'_> {
    #[inline]
    fn bit(&mut self, p: &mut Prob, _b: u32) -> u32 {
        let bound = (self.range >> PROB_BITS) * *p as u32;
        let one = self.code >= bound;
        self.code -= if one { bound } else { 0 };
        self.range = std::hint::select_unpredictable(one, self.range - bound, bound);
        let up = (ONE - *p) >> MOVE;
        let down = *p >> MOVE;
        *p = std::hint::select_unpredictable(one, *p - down, *p + up);
        while self.range < TOP {
            self.range <<= 8;
            self.code = self.code << 8 | self.next() as u32;
        }
        one as u32
    }
}

/// A decoder that can tell whether it read far past its data.
pub trait Bounded: BitCoder {
    /// True if the decoder has not read far past the data (the encoder flushes 4 bytes).
    fn in_bounds(&self) -> bool;
}

impl Bounded for Decoder<'_> {
    fn in_bounds(&self) -> bool {
        Decoder::in_bounds(self)
    }
}

/// [`Decoder`] of a stream taken in pieces ([`aqr_api::stream::Reader`]): the same bits, zeros
/// past the end.
pub struct StreamDecoder<'a> {
    code: u32,
    range: u32,
    src: aqr_api::stream::Reader<'a>,
    /// Bytes taken, past the end too.
    pos: u64,
    len: u64,
}

impl<'a> StreamDecoder<'a> {
    /// A decoder of the `len` bytes of `src`, which `src` is read in pieces of `piece` bytes.
    pub fn new(src: &'a mut dyn aqr_api::stream::Pull, len: u64, piece: usize) -> Self {
        let mut d =
            StreamDecoder { code: 0, range: u32::MAX, src: aqr_api::stream::Reader::new(src, piece), pos: 0, len };
        for _ in 0..4 {
            d.code = d.code << 8 | d.next() as u32;
        }
        d
    }

    #[inline]
    fn next(&mut self) -> u8 {
        self.pos += 1;
        self.src.byte().unwrap_or(0)
    }
}

impl Bounded for StreamDecoder<'_> {
    fn in_bounds(&self) -> bool {
        self.pos <= self.len + 4
    }
}

impl BitCoder for StreamDecoder<'_> {
    #[inline]
    fn bit(&mut self, p: &mut Prob, _b: u32) -> u32 {
        let bound = (self.range >> PROB_BITS) * *p as u32;
        let one = self.code >= bound;
        self.code -= if one { bound } else { 0 };
        self.range = std::hint::select_unpredictable(one, self.range - bound, bound);
        let up = (ONE - *p) >> MOVE;
        let down = *p >> MOVE;
        *p = std::hint::select_unpredictable(one, *p - down, *p + up);
        while self.range < TOP {
            self.range <<= 8;
            self.code = self.code << 8 | self.next() as u32;
        }
        one as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let mut seed = 3u64;
        let mut vals = Vec::new();
        for _ in 0..20000 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            vals.push(((seed >> 33) as i32 % 1000) * if seed & 1 == 0 { 1 } else { 0 });
        }
        let mut e = Encoder::default();
        let (mut p, mut t, mut s) = (P_INIT, vec![P_INIT; 64], SIntProbs::default());
        for (i, &v) in vals.iter().enumerate() {
            e.bit(&mut p, (v & 1) as u32);
            e.tree(&mut t, 6, (i % 64) as u32);
            e.sint(&mut s, v);
        }
        let out = e.finish();
        let mut d = Decoder::new(&out);
        let (mut p, mut t, mut s) = (P_INIT, vec![P_INIT; 64], SIntProbs::default());
        for (i, &v) in vals.iter().enumerate() {
            assert_eq!(d.bit(&mut p, 0), (v & 1) as u32);
            assert_eq!(d.tree(&mut t, 6, 0), (i % 64) as u32);
            assert_eq!(d.sint(&mut s, 0), v);
        }
        assert!(d.in_bounds());
    }
}
