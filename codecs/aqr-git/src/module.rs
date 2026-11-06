//! The code of the modules (see `formula`, and FORMAT.md), which needs no aqr-formula, so that
//! aqr-modules builds them into their Wasm modules.

use aqr_common::varint;

use crate::{Result, corrupt};
use crate::{index, layout};

/// The memory plan of `git-pack-recons` ([`aqr_api::stack`]): what it takes beyond its initial
/// memory at most, of inputs of `inputs` bytes (the second `layout`) and a pack of `len`
/// (`None` where they are not so): what `aqr_api::whole` copies, and what building takes
/// ([`layout::plan`]).
pub fn plan(inputs: &[u64], layout: &[u8], len: u64) -> Option<u64> {
    Some(aqr_api::stack::whole(0, &[], inputs) + layout::plan(layout, len, 0, false)?)
}

/// The memory plan of `git-pack-index-recons`, as [`plan`]'s, of params `params` and an output
/// of `len` bytes: the pack with room for the indexes after it, the entries' offsets, and what
/// the indexes take ([`index::plan`]).
pub fn index_plan(params: &[u8], inputs: &[u64], layout: &[u8], len: u64) -> Option<u64> {
    let (_, pack_len) = index_params(params).ok()?;
    let n = layout::tags(layout).ok()?.len() as u64;
    let pack = layout::plan(layout, pack_len, len.checked_sub(pack_len)?, true)?;
    Some(aqr_api::stack::whole(params.len() as u64, &[], inputs) + pack + index::plan(n))
}

/// The flags and the pack's length of [`decode_index`]'s params.
fn index_params(params: &[u8]) -> Result<(u8, u64)> {
    let &[flags, ..] = params else { return Err(corrupt()) };
    let mut at = 1;
    let pack_len = varint::get(params, &mut at)?;
    if flags & !WITH_REV != 0 || at != params.len() {
        return Err(corrupt());
    }
    Ok((flags, pack_len))
}

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
    let &[deflated, layout, kept, ids] = inputs else { return Err(corrupt()) };
    let (flags, pack_len) = index_params(params)?;
    let more = usize::try_from(len.checked_sub(pack_len).ok_or_else(corrupt)?).map_err(|_| corrupt())?;
    let mut starts = Vec::new();
    // (the indexes written in the room left past the pack)
    let mut out = layout::build_with(layout, deflated, kept, pack_len, more, Some(&mut starts))?;
    let rev = flags & WITH_REV != 0;
    if index::len(&starts, rev) != more {
        return Err(corrupt());
    }
    let at = out.len();
    out.resize(at + more, 0);
    let (pack, tail) = out.split_at_mut(at);
    index::build(pack, &starts, ids, rev, tail)?;
    Ok(out)
}
