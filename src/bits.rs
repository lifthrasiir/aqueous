//! LSB-first bit I/O as used by DEFLATE.

use crate::{Error, Result};

pub struct BitReader<'a> {
    data: &'a [u8],
    /// Position in bits.
    pos: usize,
}

impl<'a> BitReader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        BitReader { data, pos: 0 }
    }

    pub fn bit_pos(&self) -> usize {
        self.pos
    }

    /// Up to 32 bits without consuming them; bits past the end read as zero.
    pub fn peek(&self, n: u32) -> u32 {
        debug_assert!(n <= 32);
        let byte = self.pos >> 3;
        let mut w = 0u64;
        for i in 0..5 {
            w |= (*self.data.get(byte + i).unwrap_or(&0) as u64) << (8 * i);
        }
        ((w >> (self.pos & 7)) & ((1u64 << n) - 1)) as u32
    }

    pub fn consume(&mut self, n: u32) -> Result<()> {
        self.pos += n as usize;
        if self.pos > self.data.len() * 8 {
            return Err(Error::Truncated);
        }
        Ok(())
    }

    pub fn bits(&mut self, n: u32) -> Result<u32> {
        let v = self.peek(n);
        self.consume(n)?;
        Ok(v)
    }

    /// Number of bits up to the next byte boundary.
    pub fn pad_len(&self) -> u32 {
        ((8 - (self.pos & 7)) & 7) as u32
    }

    pub fn bytes(&mut self, n: usize) -> Result<&'a [u8]> {
        debug_assert!(self.pos & 7 == 0);
        let start = self.pos >> 3;
        let s = self.data.get(start..start + n).ok_or(Error::Truncated)?;
        self.pos += n * 8;
        Ok(s)
    }
}

#[derive(Default)]
pub struct BitWriter {
    out: Vec<u8>,
    acc: u64,
    nacc: u32,
}

impl BitWriter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn bits(&mut self, n: u32, v: u32) {
        debug_assert!(n <= 32 && (n == 32 || v >> n == 0));
        self.acc |= (v as u64) << self.nacc;
        self.nacc += n;
        while self.nacc >= 8 {
            self.out.push(self.acc as u8);
            self.acc >>= 8;
            self.nacc -= 8;
        }
    }

    pub fn pad_len(&self) -> u32 {
        (8 - self.nacc) & 7
    }

    pub fn bytes(&mut self, b: &[u8]) {
        debug_assert!(self.nacc == 0);
        self.out.extend_from_slice(b);
    }

    pub fn finish(self) -> Vec<u8> {
        debug_assert!(self.nacc == 0, "unpadded trailing bits");
        self.out
    }
}
