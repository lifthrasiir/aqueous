//! The code of the module (see `formula`, and `FORMAT.md`), which needs no aqr-formula, so that
//! aqr-modules builds it into its Wasm module.

use crate::{Result, corrupt, mo};
use aqr_common::varint;

/// The module: the file of `len` bytes from `params` (where the strings start, a varint) and
/// `inputs` (the file without the parts derived, and the strings' NULs).
pub fn decode(params: &[u8], inputs: &[&[u8]], len: u64) -> Result<Vec<u8>> {
    let &[rest, nuls] = inputs else { return Err(corrupt()) };
    let mut at = 0;
    let start = varint::get(params, &mut at).map_err(|_| corrupt())?;
    if at != params.len() {
        return Err(corrupt());
    }
    let (start, len) = (usize::try_from(start).map_err(|_| corrupt())?, usize::try_from(len).map_err(|_| corrupt())?);
    mo::build(rest, start, nuls, len)
}
