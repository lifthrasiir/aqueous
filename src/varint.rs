//! LEB128 with zigzag, for our streams.

#[inline]
pub fn zz(v: i64) -> u64 {
    ((v << 1) ^ (v >> 63)) as u64
}

#[inline]
pub fn unzz(v: u64) -> i64 {
    (v >> 1) as i64 ^ -((v & 1) as i64)
}

#[inline]
pub fn put_uv(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push(v as u8 | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

#[inline]
pub fn put_sv(out: &mut Vec<u8>, v: i64) {
    put_uv(out, zz(v))
}

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
        let mut v = 0u64;
        let mut s = 0;
        loop {
            let c = self.u8();
            v |= ((c & 0x7f) as u64) << s;
            if c < 0x80 || s >= 63 {
                return v;
            }
            s += 7;
        }
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
