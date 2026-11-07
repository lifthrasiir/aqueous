//! The module's streams taken in pieces: the inputs read in order ([`Pull`], [`Reader`], and
//! the data of [`crate::window::Window`]) and the output put in order ([`Put`]), so that a part
//! module holds a piece of each and nothing of their lengths (FORMAT.md, "The module"). They
//! are [`aqr_api::stream`]'s.

pub use aqr_api::stream::{Pull, Put, Reader, Slice};
use aqr_common::arith::{Bit, Coder};

/// Bytes of the pieces in which the record and the index are taken and the stream is put.
pub const PIECE: usize = 1 << 14;

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
