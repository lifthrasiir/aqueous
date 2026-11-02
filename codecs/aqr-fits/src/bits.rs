//! MSB-first bit I/O: Rice tiles ([`crate::rice`]) and the raw bits of the pixel model
//! ([`crate::image`]).

#[derive(Default)]
pub struct Writer {
    pub out: Vec<u8>,
    acc: u64,
    n: u32,
}

impl Writer {
    pub fn with_capacity(bytes: usize) -> Writer {
        Writer { out: Vec::with_capacity(bytes), acc: 0, n: 0 }
    }

    /// The low `n` bits of `v`, `n` at most 32.
    #[inline]
    pub fn put(&mut self, v: u32, n: u32) {
        debug_assert!(n <= 32);
        self.acc = self.acc << n | (v as u64 & ((1u64 << n) - 1));
        self.n += n;
        while self.n >= 8 {
            self.n -= 8;
            self.out.push((self.acc >> self.n) as u8);
        }
    }

    /// `n` zero bits.
    #[inline]
    pub fn zeros(&mut self, mut n: u32) {
        while n > 24 {
            self.put(0, 24);
            n -= 24;
        }
        self.put(0, n);
    }

    /// The bytes, the last one padded with zero bits.
    pub fn finish(mut self) -> Vec<u8> {
        if self.n > 0 {
            self.out.push((self.acc << (8 - self.n)) as u8);
        }
        self.out
    }
}

/// Reads zeros past the end.
pub type Reader<'a> = aqr_common::msb::ZeroReader<'a>;
