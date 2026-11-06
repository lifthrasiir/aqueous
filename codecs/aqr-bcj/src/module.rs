//! The code of the modules (see `formula`, and `FORMAT.md`), which needs no aqr-formula, so that
//! aqr-modules builds them into their Wasm modules. Each applies a filter's encoder to its
//! input, as the encoder of an archive did before compressing it.

use crate::bra::{self, Arch};
use crate::{Error, Result, bcj2, corrupt, delta};

/// The start address that a branch converter's parameters tell (none: 0; else le32), aligned as
/// its architecture wants.
fn start(arch: Arch, params: &[u8]) -> Result<u32> {
    let pc = match params {
        [] => 0,
        &[a, b, c, d] => u32::from_le_bytes([a, b, c, d]),
        _ => return Err(Error("bad parameters")),
    };
    if pc & arch.alignment() != 0 {
        return Err(Error("start not aligned"));
    }
    Ok(pc)
}

fn one<'a>(inputs: &[&'a [u8]], len: u64) -> Result<&'a [u8]> {
    let &[d] = inputs else { return Err(Error("one input wanted")) };
    if d.len() as u64 != len {
        return Err(Error("output of another length"));
    }
    Ok(d)
}

/// The module `bcj-{arch}`: the branch converter's encoding of the input.
pub fn bra(arch: Arch, params: &[u8], inputs: &[&[u8]], len: u64) -> Result<Vec<u8>> {
    let pc = start(arch, params)?;
    let mut d = one(inputs, len)?.to_vec();
    bra::convert(arch, &mut d, pc, true);
    Ok(d)
}

/// The module `bcj-delta`: the Delta filter's encoding of the input, the distance less one the
/// parameter byte.
pub fn delta(params: &[u8], inputs: &[&[u8]], len: u64) -> Result<Vec<u8>> {
    let &[k] = params else { return Err(Error("bad parameters")) };
    let mut d = one(inputs, len)?.to_vec();
    delta::encode(&mut d, k as usize + 1);
    Ok(d)
}

/// The pages of the windows of `bcj-delta`'s input and output (as a part module,
/// [`delta_part`]).
pub const DELTA_WINDOW: u32 = 1;

/// The module `bcj-delta` as a part module (its entry point, of `aqr_api::entry!`): its input
/// encoded a piece at a time through windows of [`DELTA_WINDOW`] pages, the bytes `dist` before
/// each kept (those before the data zeros).
pub fn delta_part(params_len: u64, in0_len: u64, size: u64) {
    use aqr_api::{Input, Memory, Output, Params};
    if params_len != 1 || size != in0_len {
        aqr_api::finish(Err(Error("bad parameters, or output of another length")));
    }
    let mut k = [0];
    Params::read(0, &mut k);
    let dist = k[0] as usize + 1;
    let (w_in, w_out) = (Input::<0>::size(), Output::<0>::size());
    let (mut hist, mut h) = ([0u8; 256], 0);
    let mut buf = [0; 4096];
    let mut pos = 0;
    while pos < size {
        let n = (size - pos).min(buf.len() as u64).min(w_in - pos % w_in).min(w_out - pos % w_out);
        let piece = &mut buf[..n as usize];
        Input::<0>::request(pos + n);
        Input::<0>::read(pos % w_in, piece);
        Input::<0>::release(pos + n);
        for b in piece.iter_mut() {
            let x = *b;
            *b = x.wrapping_sub(hist[h]);
            hist[h] = x;
            h = if h + 1 == dist { 0 } else { h + 1 };
        }
        Output::<0>::write(pos % w_out, piece);
        Output::<0>::commit(pos + n);
        pos += n;
    }
}

/// The module `bcj2`: the main, call and jump streams of the data (input 0) with the bits that
/// the range coder's stream (input 1) codes, one after another.
pub fn bcj2(params: &[u8], inputs: &[&[u8]], len: u64) -> Result<Vec<u8>> {
    let &[d, rc] = inputs else { return Err(Error("two inputs wanted")) };
    if !params.is_empty() {
        return Err(corrupt());
    }
    if d.len() as u64 != len {
        return Err(Error("output of another length"));
    }
    bcj2::encode_joined(d, rc)
}

/// The memory plans of the modules ([`aqr_api::stack`]): what each takes beyond its initial
/// memory at most, of parameters of `params` bytes, inputs of `inputs` bytes and an output of
/// `len`. A branch converter, Delta and BCJ2 (as whole modules): what `aqr_api::whole` copies,
/// and the output; Delta as a part module nothing.
pub fn plan(params: u64, inputs: &[u64], len: u64) -> u64 {
    aqr_api::stack::whole(params, &[], inputs) + aqr_api::stack::block(len)
}
