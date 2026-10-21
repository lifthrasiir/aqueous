//! Carry-less binary arithmetic coding (32-bit, after lpaq), for the adaptive profiles.

/// Carry-less binary arithmetic coder (32-bit, after lpaq).
#[derive(Default)]
pub struct ArithEncoder {
    x1: u32,
    x2: u32,
    out: Vec<u8>,
    used: bool,
}

impl ArithEncoder {
    pub fn new() -> Self {
        ArithEncoder {
            x1: 0,
            x2: u32::MAX,
            out: vec![],
            used: false,
        }
    }

    pub fn encode(&mut self, bit: bool, p1: u32) {
        self.used = true;
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

    /// Ends the stream with a single byte: any value in (x1, x2] decodes correctly, and the
    /// decoder reads zeros past the end, so `(x1 >> 24) + 1` (which the normalization keeps
    /// at most `x2 >> 24`) followed by zeros will do. An unused coder emits nothing.
    pub fn finish(mut self) -> Vec<u8> {
        if self.used {
            self.out.push((self.x1 >> 24) as u8 + 1);
        }
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
        let mut d = ArithDecoder {
            x1: 0,
            x2: u32::MAX,
            x: 0,
            input,
            pos: 0,
        };
        for _ in 0..4 {
            d.x = d.x << 8 | d.byte() as u32;
        }
        d
    }

    fn byte(&mut self) -> u8 {
        let b = self.input.get(self.pos).copied().unwrap_or(0);
        self.pos += 1;
        b
    }

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
