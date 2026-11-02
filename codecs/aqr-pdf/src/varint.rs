//! LEB128 integers and a bounds-checked reader.

use crate::{Error, Result};
use aqr_common::varint;

pub use varint::put;

/// A cursor over a byte slice; every read is checked.
pub type Reader<'a> = aqr_common::cursor::Cursor<'a>;

/// A varint of a [`Reader`] that must not exceed a bound.
pub trait Upto {
    fn upto(&mut self, max: usize) -> Result<usize>;
}

impl Upto for Reader<'_> {
    #[inline]
    fn upto(&mut self, max: usize) -> Result<usize> {
        let v = self.uv()?;
        if v > max as u64 {
            return Err(Error::Corrupt);
        }
        Ok(v as usize)
    }
}
