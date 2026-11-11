//! MSB-first bit reader and writer.
use alloc::vec::Vec;

pub struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> BitReader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        BitReader { data, pos: 0 }
    }
    pub fn pos(&self) -> usize {
        self.pos
    }
    pub fn total(&self) -> usize {
        self.data.len() * 8
    }
    pub fn left(&self) -> usize {
        self.total() - self.pos
    }
    /// The next 32 bits, left-aligned, zero past the end.
    pub fn peek32(&self) -> u32 {
        let mut v = 0u64;
        let byte = self.pos >> 3;
        for k in 0..5 {
            v = v << 8 | *self.data.get(byte + k).unwrap_or(&0) as u64;
        }
        (v >> (8 - (self.pos & 7)) & 0xFFFF_FFFF) as u32
    }
    /// `n` bits (at most 32), or `None` if the data ends first.
    pub fn get(&mut self, n: u32) -> Option<u32> {
        if n == 0 {
            return Some(0);
        }
        if self.left() < n as usize {
            return None;
        }
        let v = self.peek32() >> (32 - n);
        self.pos += n as usize;
        Some(v)
    }
    pub fn skip(&mut self, n: usize) {
        self.pos += n;
    }
}

#[derive(Default)]
pub struct BitWriter {
    out: Vec<u8>,
    acc: u64,
    n: u32,
}

impl BitWriter {
    /// Writes into `out` (cleared), which should have the capacity already.
    pub fn with_vec(mut out: Vec<u8>) -> Self {
        out.clear();
        BitWriter { out, acc: 0, n: 0 }
    }
    pub fn put(&mut self, v: u32, n: u32) {
        if n == 0 {
            return;
        }
        self.acc = self.acc << n | (v as u64 & ((1u64 << n) - 1));
        self.n += n;
        while self.n >= 8 {
            self.n -= 8;
            self.out.push((self.acc >> self.n) as u8);
        }
        self.acc &= (1u64 << self.n) - 1;
    }
    pub fn bits(&self) -> usize {
        self.out.len() * 8 + self.n as usize
    }
    /// The bytes, the last one padded with zero bits.
    pub fn finish(mut self) -> Vec<u8> {
        if self.n > 0 {
            self.out.push((self.acc << (8 - self.n)) as u8);
        }
        self.out
    }
}
