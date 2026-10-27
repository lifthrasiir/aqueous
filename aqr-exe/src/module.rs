//! The code of the modules of aqr-exe's filters (`exe-recons/x86`, `exe-recons/arm64` and
//! `exe-recons/cil`, as aqr-decompress names them): the parameters and the inputs checked, then
//! the section decoded. aqr-modules builds it into their Wasm modules.

use alloc::vec::Vec;

use crate::arch::{arm64, cil, x86};

pub type Result<T> = core::result::Result<T, &'static str>;

/// The x86 disfilter on a code section. Parameters: the origin (u64, little endian), the
/// sentinel size (u8) and 1 for x86-64 or 0 for x86-32; inputs: the streams of
/// [`x86::STREAM_NAMES`].
pub fn x86(params: &[u8], inputs: &[&[u8]], len: u64) -> Result<Vec<u8>> {
    let (Some(p), Ok(streams)) = (params.first_chunk::<10>(), <[&[u8]; x86::NUM_STREAMS]>::try_from(inputs)) else {
        return Err("bad parameters or inputs");
    };
    let origin = u64::from_le_bytes(*p.first_chunk().unwrap());
    of_len(x86::decode(&x86::DecodedStreams::new(origin, p[8], streams), p[9] != 0), len)
}

/// The ARM64 filter on a code section. Parameters: the origin (u64, little endian); inputs:
/// the streams of [`arm64::STREAM_NAMES`].
pub fn arm64(params: &[u8], inputs: &[&[u8]], len: u64) -> Result<Vec<u8>> {
    let (Some(p), Ok(streams), Ok(n)) =
        (params.first_chunk::<8>(), <[&[u8]; arm64::NUM_STREAMS]>::try_from(inputs), usize::try_from(len))
    else {
        return Err("bad parameters or inputs");
    };
    of_len(arm64::decode(streams, u64::from_le_bytes(*p), n), len)
}

/// The CIL filter on the section holding the metadata of a .NET image. Parameters: the
/// section's RVA and the offset of the metadata root in it (u32 each, little endian); inputs:
/// the streams of [`cil::STREAM_NAMES`].
pub fn cil(params: &[u8], inputs: &[&[u8]], len: u64) -> Result<Vec<u8>> {
    let (Some(p), Ok(streams), Ok(n)) =
        (params.first_chunk::<8>(), <[&[u8]; cil::NUM_STREAMS]>::try_from(inputs), usize::try_from(len))
    else {
        return Err("bad parameters or inputs");
    };
    let (origin, meta) =
        (u32::from_le_bytes(p[..4].try_into().unwrap()), u32::from_le_bytes(p[4..].try_into().unwrap()));
    of_len(cil::decode(streams, origin, meta as usize, n), len)
}

fn of_len(out: Option<Vec<u8>>, len: u64) -> Result<Vec<u8>> {
    match out {
        Some(o) if o.len() as u64 == len => Ok(o),
        Some(_) => Err("output of another length"),
        None => Err("corrupt"),
    }
}
