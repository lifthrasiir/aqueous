//! LEB128 with zigzag, for our streams.

pub use aqr_common::varint::{put as put_uv, put_signed as put_sv, unzz, zz};

/// A stream reader that never fails while reading: past the end it reads zeros, and
/// `overrun` tells afterwards whether that happened.
pub struct Rd<'a> {
    pub b: &'a [u8],
    pub p: usize,
}

impl<'a> Rd<'a> {
    pub fn new(b: &'a [u8]) -> Self {
        Rd { b, p: 0 }
    }

    #[inline]
    pub fn u8(&mut self) -> u8 {
        let c = self.b.get(self.p).copied().unwrap_or(0);
        self.p += 1;
        c
    }

    #[inline]
    pub fn uv(&mut self) -> u64 {
        aqr_common::varint::get_lenient(self.b, &mut self.p)
    }

    #[inline]
    pub fn sv(&mut self) -> i64 {
        unzz(self.uv())
    }

    /// `n` bytes, or None past the end.
    #[inline]
    pub fn bytes(&mut self, n: usize) -> Option<&'a [u8]> {
        let e = self.p.checked_add(n)?;
        let r = self.b.get(self.p..e)?;
        self.p = e;
        Some(r)
    }

    pub fn overrun(&self) -> bool {
        self.p > self.b.len()
    }

    pub fn done(&self) -> bool {
        self.p == self.b.len()
    }
}
