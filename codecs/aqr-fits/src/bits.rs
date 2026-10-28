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
pub struct Reader<'a> {
    d: &'a [u8],
    /// The position in bits.
    pub pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(d: &'a [u8]) -> Reader<'a> {
        Reader { d, pos: 0 }
    }

    #[inline]
    pub fn bit(&mut self) -> u32 {
        let b = self.d.get(self.pos >> 3).map_or(0, |&b| (b >> (7 - (self.pos & 7))) & 1);
        self.pos += 1;
        b as u32
    }

    /// `n` bits, `n` at most 32.
    #[inline]
    pub fn get(&mut self, n: u32) -> u32 {
        debug_assert!(n <= 32);
        if n == 0 {
            return 0;
        }
        // the 8 bytes from the current one, zeros past the end
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
        let v = (w << (self.pos & 7)) >> (64 - n);
        self.pos += n as usize;
        v as u32
    }

    /// Whether more bits were read than there are.
    pub fn past_end(&self) -> bool {
        self.pos > self.d.len() * 8
    }
}
