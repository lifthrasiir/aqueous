//! The code of the module `gif-recons` (see `formula`, and `FORMAT.md`), which needs no
//! aqr-formula, so that aqr-modules builds it into its Wasm module.

use aqr_common::varint;

use crate::{Error, Result, corrupt, recons};

/// The params: a varint of the pixels an image holds at most as it is decoded.
pub fn params(held: u64) -> Vec<u8> {
    let mut v = Vec::new();
    varint::put(&mut v, held);
    v
}

fn held(params: &[u8]) -> Option<u64> {
    let mut at = 0;
    let held = varint::get(params, &mut at).ok()?;
    (at == params.len() && held <= 2 * recons::MAX_PIXELS).then_some(held)
}

/// The memory plan of the module ([`aqr_api::stack`]): what it takes beyond its initial memory
/// at most, of `params`, the inputs `sk` and `record` and `len` bytes of output: what
/// `aqr_api::whole` copies, the output, and the decoder's ([`recons::plan`]). `None` for params
/// or inputs it does not take.
pub fn plan(params: &[u8], sk: &[u8], record: &[u8], len: u64) -> Option<u64> {
    use aqr_api::stack::{block, whole};
    let held = held(params)?;
    let inputs = [sk.len() as u64, record.len() as u64];
    Some(whole(params.len() as u64, &[], &inputs) + block(len) + recons::plan(sk, record, held)?)
}

/// The module: the file of `len` bytes that the inputs (skeleton, record) rebuild (see
/// `FORMAT.md`).
pub fn decode(params: &[u8], inputs: &[&[u8]], len: u64, _threads: usize) -> Result<Vec<u8>> {
    let &[skeleton, record] = inputs else { return Err(Error("two inputs wanted")) };
    let held = held(params).ok_or_else(corrupt)?;
    let len = usize::try_from(len).map_err(|_| corrupt())?;
    let mut out = Vec::with_capacity(len);
    recons::join_onto(skeleton, record, held, &mut out)?;
    if out.len() != len {
        return Err(Error("output of another length"));
    }
    Ok(out)
}
