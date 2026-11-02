//! Reading the recompressors' own streams: bytes, varints, and the bytes of a length, every
//! read checked (a length past the end, or one that overflows, is [`Corrupt`]).

use crate::{Corrupt, varint};

/// `n` bytes of `d` at `*at`, `*at` moved past them.
#[inline]
pub fn take<'a>(d: &'a [u8], at: &mut usize, n: usize) -> Result<&'a [u8], Corrupt> {
    let end = at.checked_add(n).ok_or(Corrupt("length past the end"))?;
    let s = d.get(*at..end).ok_or(Corrupt("truncated"))?;
    *at = end;
    Ok(s)
}

/// A cursor over one of the streams.
#[derive(Clone, Copy, Default, Debug)]
pub struct Cursor<'a> {
    pub d: &'a [u8],
    /// The next byte read.
    pub p: usize,
}

impl<'a> Cursor<'a> {
    #[inline]
    pub fn new(d: &'a [u8]) -> Self {
        Cursor { d, p: 0 }
    }

    #[inline]
    pub fn u8(&mut self) -> Result<u8, Corrupt> {
        let v = *self.d.get(self.p).ok_or(Corrupt("truncated"))?;
        self.p += 1;
        Ok(v)
    }

    /// A varint.
    #[inline]
    pub fn uv(&mut self) -> Result<u64, Corrupt> {
        varint::get(self.d, &mut self.p)
    }

    /// A zigzag varint.
    #[inline]
    pub fn sv(&mut self) -> Result<i64, Corrupt> {
        varint::get_signed(self.d, &mut self.p)
    }

    /// A varint as a `usize`.
    #[inline]
    pub fn usize(&mut self) -> Result<usize, Corrupt> {
        usize::try_from(self.uv()?).map_err(|_| Corrupt("length past the end"))
    }

    /// The next `n` bytes.
    #[inline]
    pub fn bytes(&mut self, n: usize) -> Result<&'a [u8], Corrupt> {
        take(self.d, &mut self.p, n)
    }

    /// The next `N` bytes.
    #[inline]
    pub fn array<const N: usize>(&mut self) -> Result<[u8; N], Corrupt> {
        Ok(*self.bytes(N)?.first_chunk().expect("N bytes"))
    }

    /// The next `n` bytes as a cursor of their own.
    #[inline]
    pub fn sub(&mut self, n: usize) -> Result<Cursor<'a>, Corrupt> {
        Ok(Cursor::new(self.bytes(n)?))
    }

    /// What is left.
    #[inline]
    pub fn rest(&self) -> &'a [u8] {
        self.d.get(self.p..).unwrap_or_default()
    }

    #[inline]
    pub fn at_end(&self) -> bool {
        self.p == self.d.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_checked() {
        let mut d = vec![7];
        varint::put(&mut d, 300);
        varint::put_signed(&mut d, -5);
        d.extend_from_slice(b"abcdef");
        let mut c = Cursor::new(&d);
        assert_eq!(c.u8(), Ok(7));
        assert_eq!(c.uv(), Ok(300));
        assert_eq!(c.sv(), Ok(-5));
        assert_eq!(c.array::<2>(), Ok(*b"ab"));
        let mut s = c.sub(2).unwrap();
        assert_eq!((s.bytes(2), s.at_end()), (Ok(&b"cd"[..]), true));
        assert_eq!(c.rest(), b"ef");
        assert!(c.bytes(3).is_err());
        assert_eq!(c.bytes(2), Ok(&b"ef"[..]));
        assert!(c.at_end() && c.u8().is_err() && c.uv().is_err());
        assert!(take(&d, &mut 1, usize::MAX).is_err());
    }
}
