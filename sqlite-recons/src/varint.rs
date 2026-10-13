//! Integer codings: SQLite's big-endian varints (in the database), and LEB128 with zigzag
//! (in our streams).

use crate::{Result, corrupt};

/// Reads a SQLite varint (1-9 bytes, big-endian 7-bit groups, the 9th byte whole).
/// Returns the value and its length.
#[inline]
pub fn get_sqlite(b: &[u8]) -> Option<(u64, usize)> {
    let mut v = 0u64;
    for i in 0..8 {
        let c = *b.get(i)?;
        v = (v << 7) | (c & 0x7f) as u64;
        if c < 0x80 {
            return Some((v, i + 1));
        }
    }
    let c = *b.get(8)?;
    Some(((v << 8) | c as u64, 9))
}

/// Length of the shortest SQLite varint for `v` (the one SQLite writes).
#[inline]
pub fn sqlite_len(v: u64) -> usize {
    if v < 1 << 7 {
        1
    } else if v < 1 << 14 {
        2
    } else if v >= 1 << 56 {
        9
    } else {
        (70 - v.leading_zeros() as usize) / 7
    }
}

/// Writes the shortest SQLite varint for `v` into `out`, returns its length.
#[inline]
pub fn put_sqlite(out: &mut [u8], v: u64) -> usize {
    if v < 0x80 {
        out[0] = v as u8;
        return 1;
    }
    if v >= 1 << 56 {
        out[8] = v as u8;
        let mut w = v >> 8;
        for i in (0..8).rev() {
            out[i] = (w & 0x7f) as u8 | 0x80;
            w >>= 7;
        }
        return 9;
    }
    let n = sqlite_len(v);
    let mut w = v;
    out[n - 1] = (w & 0x7f) as u8;
    for i in (0..n - 1).rev() {
        w >>= 7;
        out[i] = (w & 0x7f) as u8 | 0x80;
    }
    n
}

pub use recons_common::varint::{len as uv_len, put as put_uv, put_signed as put_sv, unzz, zz};

/// A cursor over one of our streams.
#[derive(Clone, Copy, Default)]
pub struct Rd<'a> {
    pub b: &'a [u8],
    pub p: usize,
}

impl<'a> Rd<'a> {
    pub fn new(b: &'a [u8]) -> Self {
        Rd { b, p: 0 }
    }

    #[inline]
    pub fn u8(&mut self) -> Result<u8> {
        let v = *self.b.get(self.p).ok_or_else(corrupt)?;
        self.p += 1;
        Ok(v)
    }

    #[inline]
    pub fn uv(&mut self) -> Result<u64> {
        recons_common::varint::get(self.b, &mut self.p).map_err(|_| corrupt())
    }

    #[inline]
    pub fn sv(&mut self) -> Result<i64> {
        Ok(unzz(self.uv()?))
    }

    #[inline]
    pub fn usize(&mut self) -> Result<usize> {
        usize::try_from(self.uv()?).map_err(|_| corrupt())
    }

    #[inline]
    pub fn bytes(&mut self, n: usize) -> Result<&'a [u8]> {
        let e = self.p.checked_add(n).ok_or_else(corrupt)?;
        let s = self.b.get(self.p..e).ok_or_else(corrupt)?;
        self.p = e;
        Ok(s)
    }

    /// The next `n` bytes as a cursor of their own.
    pub fn sub(&mut self, n: usize) -> Result<Rd<'a>> {
        Ok(Rd::new(self.bytes(n)?))
    }

    pub fn at_end(&self) -> bool {
        self.p == self.b.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sqlite_varints() {
        let mut vals = vec![0u64, 1, 127, 128, 16383, 16384, u64::MAX, 1 << 56, (1 << 56) - 1];
        for s in 0..64 {
            vals.push(1 << s);
            vals.push((1u64 << s).wrapping_sub(1));
            vals.push((1u64 << s) + 1);
        }
        for v in vals {
            let mut b = [0u8; 9];
            let n = put_sqlite(&mut b, v);
            assert_eq!(n, sqlite_len(v), "{v}");
            assert_eq!(get_sqlite(&b[..n]), Some((v, n)), "{v}");
        }
    }

    #[test]
    fn leb() {
        for v in [0u64, 1, 127, 128, 300, u64::MAX, 1 << 63] {
            let mut o = vec![];
            put_uv(&mut o, v);
            assert_eq!(o.len(), uv_len(v));
            assert_eq!(Rd::new(&o).uv().unwrap(), v);
        }
        for v in [0i64, -1, 1, i64::MIN, i64::MAX] {
            assert_eq!(unzz(zz(v)), v);
        }
    }
}
