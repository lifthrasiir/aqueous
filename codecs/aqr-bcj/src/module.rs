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
    let mut s = bcj2::encode(d, rc)?;
    s.main.reserve_exact(s.call.len() + s.jump.len());
    s.main.extend_from_slice(&s.call);
    s.main.extend_from_slice(&s.jump);
    Ok(s.main)
}
