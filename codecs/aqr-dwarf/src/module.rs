//! The code of the modules (see `formula`, and `FORMAT.md`), which needs no aqr-formula, so that
//! aqr-modules builds it into its Wasm modules.

use crate::Result;

/// `dwarf-apple`: an Apple accelerator table of `len` bytes from `params` (its header) and
/// `inputs` (`.debug_str`, then the streams of [`crate::apple::STREAM_NAMES`]).
pub fn apple(params: &[u8], inputs: &[&[u8]], len: u64) -> Result<Vec<u8>> {
    crate::apple::decode(params, inputs, len)
}
