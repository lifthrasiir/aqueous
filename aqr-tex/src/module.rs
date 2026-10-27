//! The code of the module (see `formula`, and `FORMAT.md`), which needs no aqr-formula, so that
//! aqr-modules builds it into its Wasm module.

use crate::layout::Layout;
use crate::{Result, corrupt, texture};

/// No output is larger than this many times its coded form, and 16 MiB (a texture of one
/// colour codes to about 1/5000 of its blocks), so that a few bytes of input cannot make the
/// module take much memory.
const MAX_RATIO: u64 = 8192;

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
