//! Adaptive binary range coder (LZMA style carry handling) with two-rate probabilities. One trait
//! serves both directions: `bit(p, b)` codes `b` when compressing and returns the decoded bit when
//! decompressing.
use alloc::vec::Vec;

/// Probability of a 1 bit: a fast and a slow estimate, 16 bits each.
#[derive(Clone, Copy)]
pub struct Prob {
    a: u16,
    b: u16,
}

impl Prob {
    pub const INIT: Prob = Prob { a: 1 << 15, b: 1 << 15 };
    #[inline]
    fn p12(&self) -> u32 {
        ((self.a as u32 + self.b as u32) >> 5).clamp(1, 4095)
    }
    #[inline]
    fn update(&mut self, bit: u32) {
        let t = (bit << 16) as i32 - bit as i32;
        self.a = (self.a as i32 + ((t - self.a as i32) >> 4)) as u16;
        self.b = (self.b as i32 + ((t - self.b as i32) >> 7)) as u16;
    }
}

pub trait Coder {
    /// Codes `b` with the adaptive probability `p`; returns the bit.
    fn bit(&mut self, p: &mut Prob, b: u32) -> u32;
    /// A bit with probability 1/2.
    fn flat(&mut self, b: u32) -> u32;
}

const TOP: u32 = 1 << 24;

pub struct Encoder {
    low: u64,
    range: u32,
    cache: u8,
    cache_size: u64,
    out: Vec<u8>,
}

impl Default for Encoder {
    fn default() -> Self {
        Encoder { low: 0, range: 0xFFFF_FFFF, cache: 0, cache_size: 1, out: Vec::new() }
    }
}

impl Encoder {
    pub fn new() -> Self {
        Self::default()
    }
    fn shift(&mut self) {
        if (self.low as u32) < 0xFF00_0000 || (self.low >> 32) != 0 {
            let carry = (self.low >> 32) as u8;
            let mut temp = self.cache;
            loop {
                self.out.push(temp.wrapping_add(carry));
                temp = 0xFF;
                self.cache_size -= 1;
                if self.cache_size == 0 {
                    break;
                }
            }
            self.cache = (self.low >> 24) as u8;
        }
        self.cache_size += 1;
        self.low = (self.low & 0x00FF_FFFF) << 8;
    }
    fn encode(&mut self, p0: u32, b: u32) {
        let bound = (self.range >> 12) * p0;
        if b == 0 {
            self.range = bound;
        } else {
            self.low += bound as u64;
            self.range -= bound;
        }
        while self.range < TOP {
            self.range <<= 8;
            self.shift();
        }
    }
    /// Bytes produced so far (about; the last few are still pending).
    pub fn len_so_far(&self) -> usize {
        self.out.len() + self.cache_size as usize
    }
    pub fn finish(mut self) -> Vec<u8> {
        for _ in 0..5 {
            self.shift();
        }
        self.out
    }
}

impl Coder for Encoder {
    #[inline]
    fn bit(&mut self, p: &mut Prob, b: u32) -> u32 {
        self.encode(4096 - p.p12(), b);
        p.update(b);
        b
    }
    #[inline]
    fn flat(&mut self, b: u32) -> u32 {
        self.encode(2048, b);
        b
    }
}

/// Where a decoder takes its bytes from; past the end it gets zeros.
pub trait Source {
    fn byte(&mut self) -> u8;
}

pub struct SliceSource<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> SliceSource<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        SliceSource { data, pos: 0 }
    }
}

impl Source for SliceSource<'_> {
    fn byte(&mut self) -> u8 {
        let b = *self.data.get(self.pos).unwrap_or(&0);
        self.pos += 1;
        b
    }
}

pub struct Decoder<S: Source> {
    src: S,
    range: u32,
    code: u32,
}

impl<S: Source> Decoder<S> {
    pub fn new(mut src: S) -> Self {
        src.byte(); // (the first byte the encoder writes is always 0)
        let mut code = 0u32;
        for _ in 0..4 {
            code = code << 8 | src.byte() as u32;
        }
        Decoder { src, range: 0xFFFF_FFFF, code }
    }
    fn decode(&mut self, p0: u32) -> u32 {
        let bound = (self.range >> 12) * p0;
        let b = if self.code < bound {
            self.range = bound;
            0
        } else {
            self.code -= bound;
            self.range -= bound;
            1
        };
        while self.range < TOP {
            self.range <<= 8;
            self.code = self.code << 8 | self.src.byte() as u32;
        }
        b
    }
}

impl<S: Source> Coder for Decoder<S> {
    #[inline]
    fn bit(&mut self, p: &mut Prob, _b: u32) -> u32 {
        let b = self.decode(4096 - p.p12());
        p.update(b);
        b
    }
    #[inline]
    fn flat(&mut self, _b: u32) -> u32 {
        self.decode(2048)
    }
}
