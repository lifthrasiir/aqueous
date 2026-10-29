//! The code of the modules (see `formula`, and FORMAT.md), which needs no aqr-formula, so that
//! aqr-modules builds them into their Wasm modules.

use aqr_common::varint;

use crate::{Result, corrupt};
use crate::{index, layout};

/// The module: the pack of `len` bytes rebuilt from `inputs`: the rebuilt entries' DEFLATE
/// streams with what aqr-flate's module derives of them, the layout and the bytes kept.
pub fn decode(params: &[u8], inputs: &[&[u8]], len: u64, _threads: usize) -> Result<Vec<u8>> {
    let (&[], &[deflated, layout, kept]) = (params, inputs) else { return Err(corrupt()) };
    layout::build(layout, deflated, kept, len)
}

/// Bit of the flags of [`decode_index`]'s params: a reverse index follows the index.
pub const WITH_REV: u8 = 1;

/// The module of a pack with its index: the pack of `pack_len` bytes as [`decode`] makes it of
/// the first three `inputs`, then its index and, where the flags tell, its reverse index, of
/// the ids of its entries in order (the fourth input), `len` bytes in all. Params: the flags,
/// then `pack_len` (a varint).
pub fn decode_index(params: &[u8], inputs: &[&[u8]], len: u64, _threads: usize) -> Result<Vec<u8>> {
    let (&[flags, ..], &[deflated, layout, kept, ids]) = (params, inputs) else { return Err(corrupt()) };
    let mut at = 1;
    let pack_len = varint::get(params, &mut at)?;
    if flags & !WITH_REV != 0 || at != params.len() {
        return Err(corrupt());
    }
    let more = usize::try_from(len.checked_sub(pack_len).ok_or_else(corrupt)?).map_err(|_| corrupt())?;
    let mut starts = Vec::new();
    let mut out = layout::build_with(layout, deflated, kept, pack_len, more, Some(&mut starts))?;
    let mut tail = Vec::with_capacity(more);
    index::build(&out, &starts, ids, flags & WITH_REV != 0, &mut tail)?;
    if tail.len() != more {
        return Err(corrupt());
    }
    out.extend_from_slice(&tail);
    Ok(out)
}
