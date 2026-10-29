//! The code of the module `ima-recons` (FORMAT.md §3), which needs no aqr-formula, so that
//! aqr-modules builds it into its Wasm module.

use aqr_common::varint;

use crate::codec::{self, Layout};
use crate::{Error, Result};

fn corrupt() -> Error {
    Error("corrupt input")
}

/// The params of blocks of `l`: its channels and its bytes per channel, varints.
pub fn params(l: Layout) -> Vec<u8> {
    let mut p = Vec::new();
    varint::put(&mut p, l.channels as u64);
    varint::put(&mut p, l.per_channel as u64);
    p
}

/// The module: the blocks, `len` bytes, from `params` (their layout) and `inputs` (the record).
pub fn decode(params: &[u8], inputs: &[&[u8]], len: u64) -> Result<Vec<u8>> {
    let &[record] = inputs else { return Err(corrupt()) };
    let mut at = 0;
    let channels = varint::get(params, &mut at).map_err(|_| corrupt())?;
    let per_channel = varint::get(params, &mut at).map_err(|_| corrupt())?;
    if at != params.len() || per_channel > 1 << 20 {
        return Err(corrupt());
    }
    let l = Layout::new(channels as usize, per_channel as usize).ok_or(corrupt())?;
    codec::decode(l, record, usize::try_from(len).map_err(|_| corrupt())?)
}
