//! The code of the modules (see `formula`, and `FORMAT.md`), which needs no aqr-formula, so that
//! aqr-modules builds it into their Wasm modules.

use crate::snappy::{self, Hash};
use crate::{Error, Result, corrupt, leveldb};

/// The module `snappy-recons`: a Snappy stream of `len` bytes (see `FORMAT.md`).
pub fn decode(params: &[u8], inputs: &[&[u8]], len: u64, _threads: usize) -> Result<Vec<u8>> {
    let (&[h], &[data]) = (params, inputs) else { return Err(corrupt()) };
    let hash = Hash::from_u8(h).ok_or_else(corrupt)?;
    of_len(snappy::compress_in(data, hash, cap(len))?, len)
}

/// The module `leveldb-recons`: a LevelDB table of `len` bytes (see `FORMAT.md`).
pub fn decode_table(params: &[u8], inputs: &[&[u8]], len: u64, _threads: usize) -> Result<Vec<u8>> {
    let (&[h], &[data, layout, kept]) = (params, inputs) else { return Err(corrupt()) };
    let hash = Hash::from_u8(h).ok_or_else(corrupt)?;
    of_len(leveldb::build_in(data, layout, kept, hash, cap(len))?, len)
}

/// The capacity of an output of `len` bytes (no larger but where it is not that output).
fn cap(len: u64) -> usize {
    usize::try_from(len).unwrap_or(usize::MAX)
}

fn of_len(out: Vec<u8>, len: u64) -> Result<Vec<u8>> {
    if out.len() as u64 != len {
        return Err(Error("output of another length"));
    }
    Ok(out)
}
