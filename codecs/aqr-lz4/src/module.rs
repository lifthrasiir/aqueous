//! The code of the module (see `formula`, and `FORMAT.md`), which needs no aqr-formula, so that
//! aqr-modules builds it into its Wasm module.

use aqr_common::varint;

use crate::block::{Compressor, MAX_ACCELERATION};
use crate::{Error, Result, corrupt, frame};

/// The kinds of stream, the first parameter.
pub(crate) const BLOCK: u8 = 0;
pub(crate) const FRAME: u8 = 1;

/// The memory plan of the module ([`aqr_api::stack`]): what it takes beyond its initial memory
/// at most, of parameters `params`, an input of `input` bytes and an output of `len`. What
/// `aqr_api::whole` copies; then the output, reserved before anything else of its own and
/// returned, and on top of it the compressor's table and (of a frame) each block compressed
/// apart before it is appended, taken off in turn. `None` for parameters it does not take.
pub fn plan(params: &[u8], input: u64, len: u64) -> Option<u64> {
    use aqr_api::stack::{block, whole};
    let mut p = 1;
    varint::get(params, &mut p).ok()?;
    let own = match (params.first(), &params[p.min(params.len())..]) {
        (Some(&BLOCK), []) => {
            let n = usize::try_from(input).ok()?;
            block(len.min(crate::block::bound(n) as u64)) + block(crate::block::TABLE_BYTES)
        }
        (Some(&FRAME), &[flg, bd]) if frame::encodable(flg, bd) => frame::plan(input, flg, bd, len)?,
        _ => return None,
    };
    Some(whole(params.len() as u64, &[], &[input]) + own)
}

/// The module: the LZ4 stream of `len` bytes that the compressor of `params` makes of the
/// single input (see `FORMAT.md`).
pub fn decode(params: &[u8], inputs: &[&[u8]], len: u64, _threads: usize) -> Result<Vec<u8>> {
    let &[data] = inputs else { return Err(Error("one input wanted")) };
    let mut p = 1;
    let a = varint::get(params, &mut p).map_err(|_| corrupt())?;
    if !(1..=MAX_ACCELERATION as u64).contains(&a) {
        return Err(corrupt());
    }
    // (the output no larger than `len` but where it is not that output)
    let cap = usize::try_from(len).unwrap_or(usize::MAX);
    let out = match (params.first(), &params[p.min(params.len())..]) {
        (Some(&BLOCK), []) => Compressor::compress_fast_in(data, a as u32, cap)?,
        (Some(&FRAME), &[flg, bd]) if frame::encodable(flg, bd) => {
            frame::encode_in(data, flg, bd, a as u32, None, cap)?.expect("nothing expected")
        }
        _ => return Err(corrupt()),
    };
    if out.len() as u64 != len {
        return Err(Error("output of another length"));
    }
    Ok(out)
}
