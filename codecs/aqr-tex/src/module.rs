//! The code of the module (see `formula`, and `FORMAT.md`), which needs no aqr-formula, so that
//! aqr-modules builds it into its Wasm module.

use crate::layout::Layout;
use crate::{Result, corrupt, texture};

/// No output is larger than this many times its coded form, and 16 MiB (a texture of one
/// colour codes to about 1/5000 of its blocks), so that a few bytes of input cannot make the
/// module take much memory.
const MAX_RATIO: u64 = 8192;

/// The task slots of the module (aqr-modules' `bcn-recons`), and so its threads.
pub const SLOTS: usize = 8;

/// The memory plan of the module ([`aqr_api::stack`]): the limits of its instance and its
/// tasks, its initial memory (`initial` bytes) with it, of params `params`, inputs of `inputs`
/// bytes and an output of `len` (`None` where they are not so): what `aqr_api::whole` copies,
/// and what decoding takes ([`texture::plan`]).
pub fn plan(params: &[u8], inputs: &[u64], initial: u64, len: u64) -> Option<(u64, u64)> {
    let layout = Layout::from_bytes(params).filter(|l| l.len() == len)?;
    let held = aqr_api::stack::whole(params.len() as u64, &[], inputs) + texture::decode_held(&layout);
    Some(texture::plan(&layout, initial, held))
}

/// The module: the blocks of `len` bytes of the layout of `params` from `inputs` (the first
/// stream of the coded blocks, and the streams of the stripes coded apart, packed), decoded on
/// up to `threads` threads.
pub fn decode(params: &[u8], inputs: &[&[u8]], len: u64, threads: usize) -> Result<Vec<u8>> {
    let layout = Layout::from_bytes(params).ok_or_else(corrupt)?;
    let &[main, apart] = inputs else { return Err(corrupt()) };
    let coded = (main.len() + apart.len()) as u64;
    if layout.len() != len || len > coded.saturating_mul(MAX_RATIO) + (1 << 24) {
        return Err(corrupt());
    }
    texture::decode(&layout, main, apart, threads)
}
