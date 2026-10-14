//! Stream reading, and the reference coding shared by both sides.

use crate::obj::Id;
use crate::{Result, corrupt};
use recons_common::varint;

#[derive(Clone, Copy)]
pub struct Rd<'a> {
    pub d: &'a [u8],
    pub p: usize,
}

impl<'a> Rd<'a> {
    pub fn new(d: &'a [u8]) -> Self {
        Rd { d, p: 0 }
    }
    pub fn at(d: &'a [u8], p: usize) -> Self {
        Rd { d, p }
    }
    #[inline]
    pub fn uv(&mut self) -> Result<u64> {
        Ok(varint::get(self.d, &mut self.p)?)
    }
    #[inline]
    pub fn sv(&mut self) -> Result<i64> {
        Ok(varint::get_signed(self.d, &mut self.p)?)
    }
    pub fn usize(&mut self) -> Result<usize> {
        usize::try_from(self.uv()?).map_err(|_| corrupt())
    }
    #[inline]
    pub fn u8(&mut self) -> Result<u8> {
        let b = *self.d.get(self.p).ok_or_else(corrupt)?;
        self.p += 1;
        Ok(b)
    }
    pub fn bytes(&mut self, n: usize) -> Result<&'a [u8]> {
        let b = self.d.get(self.p..self.p.checked_add(n).ok_or_else(corrupt)?).ok_or_else(corrupt)?;
        self.p += n;
        Ok(b)
    }
    pub fn at_end(&self) -> bool {
        self.p == self.d.len()
    }
}

pub use varint::{put as put_uv, put_signed as put_sv};

/// A reference as decoded: an index into the table, or an id not in it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ref {
    Idx(u32),
    Id(Id),
}

/// A binary reference: 0 then the 20 bytes when `j` is `None`, else `zz(j - cur) + 1`;
/// `cur` follows the references.
pub fn put_ref(out: &mut Vec<u8>, j: Option<u32>, id: &[u8], cur: &mut i64) {
    match j {
        Some(j) => {
            put_uv(out, varint::zz(j as i64 - *cur) + 1);
            *cur = j as i64;
        }
        None => {
            out.push(0);
            out.extend_from_slice(id);
        }
    }
}

pub fn get_ref(r: &mut Rd, cur: &mut i64, n: usize) -> Result<Ref> {
    let v = r.uv()?;
    if v == 0 {
        return Ok(Ref::Id(r.bytes(20)?.try_into().unwrap()));
    }
    let j = *cur + varint::unzz(v - 1);
    if j < 0 || j as usize >= n {
        return Err(corrupt());
    }
    *cur = j;
    Ok(Ref::Idx(j as u32))
}
