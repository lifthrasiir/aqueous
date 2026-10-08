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

#[inline]
pub fn uv_len(v: u64) -> usize {
    (64 - (v | 1).leading_zeros() as usize).div_ceil(7)
}

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
        // Fast path: one byte.
        if let Some(&c) = self.b.get(self.p)
            && c < 0x80
        {
            self.p += 1;
            return Ok(c as u64);
        }
        let mut v = 0u64;
        let mut shift = 0;
        loop {
            let c = *self.b.get(self.p).ok_or_else(corrupt)?;
            self.p += 1;
            v |= ((c & 0x7f) as u64).checked_shl(shift).ok_or_else(corrupt)?;
            if c < 0x80 {
                return Ok(v);
            }
            shift += 7;
            if shift >= 64 {
                return Err(corrupt());
            }
        }
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
