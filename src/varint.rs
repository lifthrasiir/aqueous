//! LEB128 integers and a bounds-checked reader.

use crate::{Error, Result};

pub fn put(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push(v as u8 | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

/// A cursor over a byte slice; every read is checked.
pub struct Reader<'a> {
    pub s: &'a [u8],
    pub pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(s: &'a [u8]) -> Self {
        Reader { s, pos: 0 }
    }

    pub fn u8(&mut self) -> Result<u8> {
        let b = *self.s.get(self.pos).ok_or(Error::Corrupt)?;
        self.pos += 1;
        Ok(b)
    }

    pub fn varint(&mut self) -> Result<u64> {
        let mut v = 0u64;
        for shift in (0..64).step_by(7) {
            let b = self.u8()?;
            v |= ((b & 0x7f) as u64) << shift;
            if b < 0x80 {
                return Ok(v);
            }
        }
        Err(Error::Corrupt)
    }

    /// A varint that must not exceed `max`.
    pub fn usize(&mut self, max: usize) -> Result<usize> {
        let v = self.varint()?;
        if v > max as u64 {
            return Err(Error::Corrupt);
        }
        Ok(v as usize)
    }

    pub fn bytes(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.pos.checked_add(n).ok_or(Error::Corrupt)?;
        let b = self.s.get(self.pos..end).ok_or(Error::Corrupt)?;
        self.pos = end;
        Ok(b)
    }

    pub fn u64le(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.bytes(8)?.try_into().unwrap()))
    }

    pub fn rest(&self) -> &'a [u8] {
        &self.s[self.pos..]
    }

    pub fn at_end(&self) -> bool {
        self.pos == self.s.len()
    }
}
