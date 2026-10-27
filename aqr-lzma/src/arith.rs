//! Carry-less binary arithmetic coding (32-bit), and adaptive bits for the record.

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
