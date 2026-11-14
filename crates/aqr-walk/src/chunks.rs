//! The table at the start of a coded stream cut in chunks, each coded on its own: the number of chunks, then for each two
//! numbers (what it holds and the bytes it is coded in), as LEB128.
use crate::Error;
use crate::reader::Reader;
use alloc::vec::Vec;

/// The most chunks of a table.
const MAX: u64 = 1 << 24;

/// The count of a table in a stream of `size` bytes: each entry takes two bytes at least.
fn count(n: u64, size: u64) -> Result<usize, Error> {
    if n > MAX || n > size / 2 {
        return Err(Error::Parse);
    }
    Ok(n as usize)
}

/// Reads the table from `r`, whose stream is `size` bytes.
pub fn read(r: &mut Reader, size: u64) -> Result<Vec<(u64, u64)>, Error> {
    let mut one = || r.varint()?.ok_or(Error::Parse);
    let n = count(one()?, size)?;
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        let a = one()?;
        let b = one()?;
        out.push((a, b));
    }
    Ok(out)
}

/// Reads the table from `data` at `*pos`.
#[cfg(not(target_family = "wasm"))]
pub fn read_at(data: &[u8], pos: &mut usize) -> Result<Vec<(u64, u64)>, Error> {
    use aqr_common::varint::get;
    let mut one = || get(data, pos).map_err(|_| Error::Parse);
    let n = count(one()?, data.len() as u64)?;
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        let a = one()?;
        let b = one()?;
        out.push((a, b));
    }
    Ok(out)
}

/// Writes the table of `entries` to `out`.
#[cfg(not(target_family = "wasm"))]
pub fn put(out: &mut Vec<u8>, entries: impl ExactSizeIterator<Item = (u64, u64)>) {
    aqr_common::varint::put(out, entries.len() as u64);
    for (a, b) in entries {
        aqr_common::varint::put(out, a);
        aqr_common::varint::put(out, b);
    }
}
