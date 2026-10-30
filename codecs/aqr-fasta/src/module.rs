//! The code of the module (see `formula`, and `FORMAT.md`), which needs no aqr-formula, so that
//! aqr-modules builds it into its Wasm module.

use crate::fasta::{self, View};
use crate::{Result, bases, corrupt};
use aqr_common::varint;

/// The module: the file of `len` bytes from `params` (the line width, whether it ends with a
/// line's end) and `inputs` (the streams of `STREAM_NAMES`).
pub fn decode(params: &[u8], inputs: &[&[u8]], len: u64, _threads: usize) -> Result<Vec<u8>> {
    let &[headers, layout, case, other, coded] = inputs else { return Err(corrupt()) };
    let mut at = 0;
    let width = varint::get(params, &mut at).map_err(|_| corrupt())?;
    let &[nl] = &params[at..] else { return Err(corrupt()) };
    if nl > 1 {
        return Err(corrupt());
    }
    let len = usize::try_from(len).map_err(|_| corrupt())?;
    let n = fasta::bases_len(layout, other)?;
    // (no more bases than bytes of the file)
    if n > len as u64 {
        return Err(corrupt());
    }
    // (the bases as they are decoded: a file's worth of them not held)
    let mut bases = bases::Decoder::new(coded, n as usize)?;
    let out = fasta::build(View { width, final_newline: nl == 1, headers, layout, case, other }, &mut bases, len)?;
    bases.finish()?;
    Ok(out)
}
