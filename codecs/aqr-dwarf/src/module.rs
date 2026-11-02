//! The code of the modules (see `formula`, and `FORMAT.md`), which needs no aqr-formula, so that
//! aqr-modules builds it into its Wasm modules.

use crate::Result;

/// `dwarf-apple`: an Apple accelerator table of `len` bytes from `params` (its header) and
/// `inputs` (`.debug_str`, then the streams of [`crate::apple::STREAM_NAMES`]).
pub fn apple(params: &[u8], inputs: &[&[u8]], len: u64) -> Result<Vec<u8>> {
    crate::apple::decode(params, inputs, len)
}

/// `dwarf-info`: `.debug_info` of `len` bytes from `params` (none) and `inputs` (`.debug_abbrev`,
/// `.debug_str`, then the inputs of [`crate::info::INPUTS`]).
pub fn info(params: &[u8], inputs: &[&[u8]], len: u64) -> Result<Vec<u8>> {
    use crate::info::{INPUTS, NUM_STREAMS};
    let &[abbrev, strs, ref packed @ ..] = inputs else { return Err(crate::corrupt()) };
    if !params.is_empty() || packed.len() != INPUTS.len() {
        return Err(crate::corrupt());
    }
    let mut streams: Vec<&[u8]> = vec![&[]; NUM_STREAMS];
    for (input, (_, which)) in packed.iter().zip(INPUTS) {
        for (s, &k) in crate::unpack(input, which.len())?.into_iter().zip(which) {
            streams[k] = s;
        }
    }
    let all: Vec<&[u8]> = [abbrev, strs].into_iter().chain(streams).collect();
    crate::info::decode(&all, len)
}
