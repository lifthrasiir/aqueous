//! The bit stream that FSE and Huffman write: bits added from the least significant end, read
//! backwards from the end mark (RFC 8878, "Bitstream").

pub(crate) struct Bits<'a> {
    out: &'a mut Vec<u8>,
    start: usize,
    acc: u64,
    n: u32,
}

impl<'a> Bits<'a> {
    pub fn new(out: &'a mut Vec<u8>) -> Self {
        let start = out.len();
        Bits { out, start, acc: 0, n: 0 }
    }

    /// The low `nb` bits of `v` (`nb` at most 32).
    #[inline]
    pub fn add(&mut self, v: u32, nb: u32) {
        debug_assert!(nb <= 32);
        let v = v as u64 & ((1u64 << nb) - 1);
        self.acc |= v << self.n;
        self.n += nb;
        if self.n >= 32 {
            self.out.extend_from_slice(&(self.acc as u32).to_le_bytes());
            self.acc >>= 32;
            self.n -= 32;
        }
    }

    /// The end mark, then the last bytes: the bytes written.
    pub fn close(mut self) -> usize {
        self.add(1, 1);
        while self.n >= 8 {
            self.out.push(self.acc as u8);
            self.acc >>= 8;
            self.n -= 8;
        }
        if self.n > 0 {
            self.out.push(self.acc as u8);
        }
        self.out.len() - self.start
    }
}
