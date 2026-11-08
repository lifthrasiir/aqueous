//! The code of the module (see `formula`, and `FORMAT.md`), which needs no aqr-formula, so that
//! aqr-modules builds it into its Wasm module.

use aqr_common::varint;

use crate::block::{Compressor, MAX_ACCELERATION};
use crate::{Error, Result, corrupt, frame};

/// The kinds of stream, the first parameter.
pub(crate) const BLOCK: u8 = 0;
pub(crate) const FRAME: u8 = 1;

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
