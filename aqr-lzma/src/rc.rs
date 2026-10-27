//! LZMA's range coder: 11-bit probabilities adapted by 1/32, and bits of probability 1/2.

pub const BITS: u32 = 11;
pub const ONE: u16 = 1 << BITS;
pub const HALF: u16 = ONE / 2;
const MOVE: u32 = 5;
const TOP: u32 = 1 << 24;

/// The side of the coder the model runs on: a decoder ignores the bit it is given and returns
/// the one it reads, an encoder writes the bit it is given and returns it.
pub trait Bits {
    fn bit(&mut self, p: &mut u16, b: u32) -> u32;
    /// `n` bits of probability 1/2, highest first.
    fn direct(&mut self, n: u32, v: u32) -> u32;
}

pub struct Encoder {
    low: u64,
    range: u32,
    cache: u8,
    pending: u64,
    pub out: Vec<u8>,
}

impl Encoder {
    pub fn new(out: Vec<u8>) -> Self {
        Encoder { low: 0, range: u32::MAX, cache: 0, pending: 1, out }
    }

    fn shift_low(&mut self) {
        if (self.low as u32) < 0xff00_0000 || self.low >> 32 != 0 {
            let carry = (self.low >> 32) as u8;
            let mut b = self.cache;
            loop {
                self.out.push(b.wrapping_add(carry));
                b = 0xff;
                self.pending -= 1;
                if self.pending == 0 {
                    break;
                }
            }
            self.cache = (self.low >> 24) as u8;
        }
        self.pending += 1;
        self.low = (self.low & 0x00ff_ffff) << 8;
    }

    pub fn finish(mut self) -> Vec<u8> {
        for _ in 0..5 {
            self.shift_low();
        }
        self.out
    }
}

impl Bits for Encoder {
    #[inline]
    fn bit(&mut self, p: &mut u16, b: u32) -> u32 {
        let bound = (self.range >> BITS) * *p as u32;
        if b == 0 {
            self.range = bound;
            *p += (ONE - *p) >> MOVE;
        } else {
            self.low += bound as u64;
            self.range -= bound;
            *p -= *p >> MOVE;
        }
        while self.range < TOP {
            self.range <<= 8;
            self.shift_low();
        }
        b
    }

    fn direct(&mut self, n: u32, v: u32) -> u32 {
        for i in (0..n).rev() {
            self.range >>= 1;
            if v >> i & 1 != 0 {
                self.low += self.range as u64;
            }
            while self.range < TOP {
                self.range <<= 8;
                self.shift_low();
            }
        }
        v
    }
}

pub struct Decoder<'a> {
    range: u32,
    code: u32,
    input: &'a [u8],
    pub pos: usize,
    /// Bytes read past the end of the input (as zeros).
    pub overrun: usize,
}

impl<'a> Decoder<'a> {
    /// None where the stream does not start with the zero byte every encoder writes first.
    pub fn new(input: &'a [u8]) -> Option<Self> {
        if input.len() < 5 || input[0] != 0 {
            return None;
        }
        let code = u32::from_be_bytes(input[1..5].try_into().unwrap());
        Some(Decoder { range: u32::MAX, code, input, pos: 5, overrun: 0 })
    }

    #[inline]
    fn byte(&mut self) -> u8 {
        let b = match self.input.get(self.pos) {
            Some(&b) => b,
            None => {
                self.overrun += 1;
                0
            }
        };
        self.pos += 1;
        b
    }

    /// Whether the coder ended where an encoder's flush would have left it.
    pub fn finished(&self) -> bool {
        self.code == 0
    }
}

impl Bits for Decoder<'_> {
    #[inline]
    fn bit(&mut self, p: &mut u16, _: u32) -> u32 {
        let bound = (self.range >> BITS) * *p as u32;
        let b = if self.code < bound {
            self.range = bound;
            *p += (ONE - *p) >> MOVE;
            0
        } else {
            self.range -= bound;
            self.code -= bound;
            *p -= *p >> MOVE;
            1
        };
        if self.range < TOP {
            self.range <<= 8;
            self.code = self.code << 8 | self.byte() as u32;
        }
        b
    }

    fn direct(&mut self, n: u32, _: u32) -> u32 {
        let mut v = 0;
        for _ in 0..n {
            self.range >>= 1;
            let b = (self.code >= self.range) as u32;
            if b != 0 {
                self.code -= self.range;
            }
            v = v << 1 | b;
            if self.range < TOP {
                self.range <<= 8;
                self.code = self.code << 8 | self.byte() as u32;
            }
        }
        v
    }
}
