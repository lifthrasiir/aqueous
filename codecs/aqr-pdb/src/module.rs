//! The code of the modules (see `formula`, and `FORMAT.md`), which needs no aqr-formula, so that
//! aqr-modules builds it into its Wasm modules.

use crate::{Result, corrupt};

/// `pdb-msf`: the file of `len` bytes from `params` (none) and `inputs` (those of
/// [`crate::msf::INPUT_NAMES`]).
pub fn msf(params: &[u8], inputs: &[&[u8]], len: u64) -> Result<Vec<u8>> {
    if !params.is_empty() {
        return Err(corrupt());
    }
    crate::msf::decode(inputs, len)
}

/// `pdb-symbols`: the modules' symbol substreams, one after another, from `params` (none) and
/// `inputs` (those of [`crate::symbols::INPUTS`]), `len` bytes in all.
pub fn symbols(params: &[u8], inputs: &[&[u8]], len: u64) -> Result<Vec<u8>> {
    use crate::symbols::{INPUTS, NUM_STREAMS};
    if !params.is_empty() || inputs.len() != INPUTS.len() {
        return Err(corrupt());
    }
    let mut streams: Vec<&[u8]> = vec![&[]; NUM_STREAMS];
    for (input, (_, which)) in inputs.iter().zip(INPUTS) {
        for (s, &k) in crate::unpack(input, which.len())?.into_iter().zip(which) {
            streams[k] = s;
        }
    }
    let out = crate::symbols::decode_sized(&streams, usize::try_from(len).map_err(|_| corrupt())?)?;
    if out.len() as u64 != len {
        return Err(corrupt());
    }
    Ok(out)
}
