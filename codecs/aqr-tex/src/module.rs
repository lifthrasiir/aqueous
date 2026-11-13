//! The code of the modules (see `formula`, and `FORMAT.md`), which needs no aqr-formula, so that
//! aqr-modules builds them into their Wasm modules: one of each group of formats
//! ([`crate::layout::Group`]), each taking the layouts of its formats only.

use crate::bcn::Bcn;
use crate::layout::Layout;
use crate::texture::Family;
use crate::{Result, corrupt, texture};

/// No output is larger than this many times its coded form, and 16 MiB (a texture of one
/// colour codes to about 1/5000 of its blocks), so that a few bytes of input cannot make the
/// module take much memory.
const MAX_RATIO: u64 = 8192;

/// The layout of params `params`, of a format of family `F` and of `len` bytes.
fn layout<F: Family>(params: &[u8], len: u64) -> Option<Layout> {
    Layout::from_bytes(params).filter(|l| l.format.group() == F::GROUP && l.len_in(F::shape(l.format)) == len)
}

/// The memory plan of family `F`'s module ([`aqr_api::stack`]): the limits of its instance and
/// its tasks, its initial memory (`initial` bytes) with it, of params `params`, inputs of
/// `inputs` bytes and an output of `len` (`None` where they are not so): what
/// `aqr_api::whole` copies, and what decoding takes ([`texture::plan`]).
pub fn plan_of<F: Family>(params: &[u8], inputs: &[u64], initial: u64, len: u64) -> Option<(u64, u64)> {
    let layout = layout::<F>(params, len)?;
    let held = aqr_api::stack::whole(params.len() as u64, &[], inputs) + texture::decode_held::<F>(&layout);
    Some(texture::plan::<F>(&layout, initial, held))
}

/// Family `F`'s module: the blocks of `len` bytes of the layout of `params` from `inputs` (the
/// first stream of the coded blocks, and the streams of the stripes coded apart, packed),
/// decoded on up to `threads` threads.
pub fn decode_of<F: Family>(params: &[u8], inputs: &[&[u8]], len: u64, threads: usize) -> Result<Vec<u8>> {
    let layout = layout::<F>(params, len).ok_or_else(corrupt)?;
    let &[main, apart] = inputs else { return Err(corrupt()) };
    let coded = (main.len() + apart.len()) as u64;
    if len > coded.saturating_mul(MAX_RATIO) + (1 << 24) {
        return Err(corrupt());
    }
    texture::decode::<F>(&layout, main, apart, threads)
}

/// `bcn-recons` ([`decode_of`]).
pub fn decode(params: &[u8], inputs: &[&[u8]], len: u64, threads: usize) -> Result<Vec<u8>> {
    decode_of::<Bcn>(params, inputs, len, threads)
}

/// `bc7-recons` ([`decode_of`]).
pub fn decode_bc7(params: &[u8], inputs: &[&[u8]], len: u64, threads: usize) -> Result<Vec<u8>> {
    decode_of::<crate::bc7::Bc7>(params, inputs, len, threads)
}

/// `bc6h-recons` ([`decode_of`]).
pub fn decode_bc6h(params: &[u8], inputs: &[&[u8]], len: u64, threads: usize) -> Result<Vec<u8>> {
    decode_of::<crate::bc6h::Bc6h>(params, inputs, len, threads)
}

/// `etc-recons` ([`decode_of`]).
pub fn decode_etc(params: &[u8], inputs: &[&[u8]], len: u64, threads: usize) -> Result<Vec<u8>> {
    decode_of::<crate::etcm::Etc>(params, inputs, len, threads)
}

/// `pvrtc-recons` ([`decode_of`]).
pub fn decode_pvrtc(params: &[u8], inputs: &[&[u8]], len: u64, threads: usize) -> Result<Vec<u8>> {
    decode_of::<crate::pvrtc::Pvrtc>(params, inputs, len, threads)
}

/// `astc-recons`'s plan ([`plan_of`]).
pub fn plan_astc(params: &[u8], inputs: &[u64], initial: u64, len: u64) -> Option<(u64, u64)> {
    plan_of::<crate::astcm::Astc>(params, inputs, initial, len)
}

/// `astc-recons` ([`decode_of`]).
pub fn decode_astc(params: &[u8], inputs: &[&[u8]], len: u64, threads: usize) -> Result<Vec<u8>> {
    decode_of::<crate::astcm::Astc>(params, inputs, len, threads)
}
