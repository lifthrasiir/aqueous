//! The code of the module `vorbis-recons` (FORMAT.md §3), which needs no aqr-formula, so that
//! aqr-modules builds it into its Wasm module.

use aqr_common::varint;

use crate::setup::{Ident, Setup};
use crate::{Error, Result, codec};

fn corrupt() -> Error {
    Error("corrupt input")
}

/// The params of a stream of `ident`: its channels (a varint), then its block sizes' exponents,
/// the short one's in the low 4 bits of a byte, the long one's in the high 4.
pub fn params(ident: Ident) -> Vec<u8> {
    let mut p = Vec::new();
    varint::put(&mut p, ident.channels as u64);
    let exp = |b: usize| b.trailing_zeros() as u8;
    p.push(exp(ident.blocksizes[0]) | exp(ident.blocksizes[1]) << 4);
    p
}

fn ident(params: &[u8]) -> Result<Ident> {
    let mut at = 0;
    let channels = varint::get(params, &mut at).map_err(|_| corrupt())?;
    let &[sizes] = &params[at..] else { return Err(corrupt()) };
    let (b0, b1) = (sizes & 15, sizes >> 4);
    if !(1..=255).contains(&channels) || !(6..=13).contains(&b0) || !(6..=13).contains(&b1) || b0 > b1 {
        return Err(corrupt());
    }
    Ok(Ident { channels: channels as usize, blocksizes: [1 << b0, 1 << b1] })
}

/// The module: the packets, each after its 16-bit length, `len` bytes, from `params` (the
/// stream's channels and block sizes) and `inputs` (the setup header and the record).
pub fn decode(params: &[u8], inputs: &[&[u8]], len: u64) -> Result<Vec<u8>> {
    let &[setup, record] = inputs else { return Err(corrupt()) };
    let setup = Setup::parse(setup, ident(params)?)?;
    codec::decode(&setup, record, usize::try_from(len).map_err(|_| corrupt())?)
}
