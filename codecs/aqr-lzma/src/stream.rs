//! The module's streams taken in pieces: the inputs read in order ([`Pull`], [`Reader`], and
//! the data of [`crate::window::Window`]) and the output put in order ([`Put`]), so that a part
//! module holds a piece of each and nothing of their lengths (FORMAT.md, "The module").

use aqr_common::arith::{Bit, Coder};

use crate::{Result, corrupt};

/// Bytes of the pieces in which the record and the index are taken and the stream is put.
pub const PIECE: usize = 1 << 14;

/// A stream of bytes taken in order.
pub trait Pull {
    /// Copies the next bytes into the front of `buf` (at least one, as many as it has room for
    /// at most) and tells how many; 0 where the stream has ended.
    fn pull(&mut self, buf: &mut [u8]) -> usize;
}

/// A byte slice as a [`Pull`], natively.
pub struct Slice<'a>(pub &'a [u8]);

impl Pull for Slice<'_> {
    fn pull(&mut self, buf: &mut [u8]) -> usize {
        let n = self.0.len().min(buf.len());
        buf[..n].copy_from_slice(&self.0[..n]);
        self.0 = &self.0[n..];
        n
    }
}

/// A stream put in order.
pub trait Put {
    /// Appends `d`. None where there is no room for it (the output is of another length).
    fn put(&mut self, d: &[u8]) -> Option<()>;
}

/// A vector as a [`Put`], natively.
impl Put for Vec<u8> {
    fn put(&mut self, d: &[u8]) -> Option<()> {
        self.extend_from_slice(d);
        Some(())
    }
}

/// A stream read a byte at a time out of a piece of it of `size` bytes.
pub struct Reader<'a> {
    src: &'a mut dyn Pull,
    buf: Vec<u8>,
    at: usize,
    end: usize,
    taken: u64,
}

impl<'a> Reader<'a> {
    pub fn new(src: &'a mut dyn Pull, size: usize) -> Reader<'a> {
        Reader { src, buf: vec![0; size], at: 0, end: 0, taken: 0 }
    }

    /// The bytes read (or dropped) so far.
    pub fn taken(&self) -> u64 {
        self.taken
    }

    /// Takes the next piece, if any is left; whether it did.
    #[cold]
    fn refill(&mut self) -> bool {
        self.at = 0;
        self.end = self.src.pull(&mut self.buf);
        self.end > 0
    }

    /// The next byte, or `None` at the end of the stream.
    #[inline(always)]
    pub fn byte(&mut self) -> Option<u8> {
        if self.at == self.end && !self.refill() {
            return None;
        }
        let b = self.buf[self.at];
        self.at += 1;
        self.taken += 1;
        Some(b)
    }

    /// Whether every byte was read.
    pub fn done(&mut self) -> bool {
        self.at == self.end && !self.refill()
    }

    /// Drops the next `n` bytes; None where the stream ends before.
    pub fn skip(&mut self, mut n: u64) -> Option<()> {
        while n > 0 {
            if self.at == self.end && !self.refill() {
                return None;
            }
            let k = ((self.end - self.at) as u64).min(n);
            self.at += k as usize;
            self.taken += k;
            n -= k;
        }
        Some(())
    }

    /// The next varint (at most ten bytes; bits beyond 64 are an error).
    pub fn num(&mut self) -> Result<u64> {
        let mut v = 0u64;
        for shift in (0..64).step_by(7) {
            let b = self.byte().ok_or_else(corrupt)?;
            if shift == 63 && b > 1 {
                return Err(corrupt());
            }
            v |= ((b & 0x7f) as u64) << shift;
            if b < 0x80 {
                return Ok(v);
            }
        }
        Err(corrupt())
    }
}

/// The range decoder of a record ([`aqr_common::arith::ArithDecoder`]) over the next `len`
/// bytes of a reader, which it reads as it goes (zeros beyond them, as the decoder over a
/// slice does).
pub struct Body<'a, 'b> {
    x1: u32,
    x2: u32,
    x: u32,
    r: &'b mut Reader<'a>,
    /// The bytes of the body not taken from the reader yet, and the bytes taken in all
    /// (counting those beyond the body, which are zeros).
    left: u64,
    pos: u64,
    len: u64,
    /// Whether the reader ended before the body did.
    short: bool,
}

impl<'a, 'b> Body<'a, 'b> {
    pub fn new(r: &'b mut Reader<'a>, len: u64) -> Self {
        let mut d = Body { x1: 0, x2: u32::MAX, x: 0, r, left: len, pos: 0, len, short: false };
        for _ in 0..4 {
            d.x = d.x << 8 | d.byte() as u32;
        }
        d
    }

    #[inline]
    fn byte(&mut self) -> u8 {
        self.pos += 1;
        if self.left == 0 {
            return 0;
        }
        self.left -= 1;
        self.r.byte().unwrap_or_else(|| {
            self.short = true;
            0
        })
    }

    #[inline]
    fn decode(&mut self, p1: u32) -> bool {
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

    fn overrun(&self) -> bool {
        self.pos > self.len + 4 || self.short
    }

    /// Ends the body: the reader goes on past all of it. None where it was read past its end.
    pub fn finish(self) -> Option<()> {
        let left = self.left;
        let ok = !self.overrun();
        self.r.skip(left)?;
        ok.then_some(())
    }
}

impl Coder for Body<'_, '_> {
    #[inline]
    fn bit(&mut self, b: &mut Bit, _: bool) -> bool {
        let v = self.decode(b.p1());
        b.update(v);
        v
    }

    #[inline]
    fn raw(&mut self, _: bool) -> bool {
        self.decode(1 << 15)
    }

    fn failed(&self) -> bool {
        self.overrun()
    }
}
