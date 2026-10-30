//! The code of the modules of aqr-exe (`exe-recons/x86`, `exe-recons/x86-remap`,
//! `exe-recons/arm64`, `exe-recons/arm64-remap`, `exe-recons/cil`, `exe-recons/pe-reloc` and
//! `exe-recons/macho-symtab`, as aqr-decompress names them): the parameters and the inputs
//! checked, then the section or table decoded. aqr-modules builds it into their Wasm modules.

use alloc::vec::Vec;

use crate::arch::{arm64, cil, x86};
use crate::{reloc, symtab};

pub type Result<T> = core::result::Result<T, &'static str>;

/// The x86 disfilter on a code section. Parameters: the origin (u64, little endian), the
/// sentinel size (u8) and 1 for x86-64 or 0 for x86-32; resources: the opcode classes
/// ([`x86::Opcodes`]); inputs: the streams of [`x86::STREAM_NAMES`].
pub fn x86(params: &[u8], resources: &[&[u8]], inputs: &[&[u8]], len: u64) -> Result<Vec<u8>> {
    let (Some(p), Ok(streams)) = (params.first_chunk::<10>(), <[&[u8]; x86::NUM_STREAMS]>::try_from(inputs)) else {
        return Err("bad parameters or inputs");
    };
    let Some(opcodes) = (match resources {
        [r] => x86::Opcodes::from_resource(r),
        _ => None,
    }) else {
        return Err("bad resources");
    };
    let origin = u64::from_le_bytes(*p.first_chunk().unwrap());
    // (no larger but where it is not the output)
    let cap = usize::try_from(len).unwrap_or(usize::MAX);
    of_len(x86::decode_with(&x86::DecodedStreams::new(origin, p[8], streams), p[9] != 0, &opcodes, cap), len)
}

/// The x86 disfilter on a code section, its references remapped to another image's.
/// Parameters, resources: as [`x86`]; inputs: the streams of [`x86::remap::STREAM_NAMES`].
pub fn x86_remap(params: &[u8], resources: &[&[u8]], inputs: &[&[u8]], len: u64) -> Result<Vec<u8>> {
    let (Some(p), Ok(streams)) = (params.first_chunk::<10>(), <[&[u8]; x86::remap::NUM_STREAMS]>::try_from(inputs))
    else {
        return Err("bad parameters or inputs");
    };
    let Some(opcodes) = (match resources {
        [r] => x86::Opcodes::from_resource(r),
        _ => None,
    }) else {
        return Err("bad resources");
    };
    let origin = u64::from_le_bytes(*p.first_chunk().unwrap());
    // (no larger but where it is not the output)
    let cap = usize::try_from(len).unwrap_or(usize::MAX);
    of_len(x86::remap::decode(origin, p[8], streams, p[9] != 0, &opcodes, cap), len)
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

/// The ARM64 filter on a code section, its BL and ADRP targets remapped to another image's.
/// Parameters: the origin (u64, little endian); inputs: the streams of
/// [`arm64::remap::STREAM_NAMES`].
pub fn arm64_remap(params: &[u8], inputs: &[&[u8]], len: u64) -> Result<Vec<u8>> {
    let (Some(p), Ok(streams), Ok(n)) =
        (params.first_chunk::<8>(), <[&[u8]; arm64::remap::NUM_STREAMS]>::try_from(inputs), usize::try_from(len))
    else {
        return Err("bad parameters or inputs");
    };
    of_len(arm64::remap::decode(streams, u64::from_le_bytes(*p), n), len)
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

/// The base relocations of a PE image. Parameters: `log2` of what the blocks' sizes are
/// multiples of (u8, 2 to 12); inputs: the streams of [`reloc::STREAM_NAMES`].
pub fn reloc(params: &[u8], inputs: &[&[u8]], len: u64) -> Result<Vec<u8>> {
    let (&[align @ 2..=12], &[rvas, types], Ok(n)) = (params, inputs, usize::try_from(len)) else {
        return Err("bad parameters or inputs");
    };
    of_len(reloc::decode(rvas, types, align, n), len)
}

/// The symbol table of a 64-bit Mach-O image and its string table after it. Parameters: the
/// number of entries and the first external one (u32 each, little endian), then the string
/// table's head; inputs: the streams of [`symtab::STREAM_NAMES`].
pub fn macho_symtab(params: &[u8], inputs: &[&[u8]], len: u64) -> Result<Vec<u8>> {
    let (Ok(streams), Ok(n)) = (<[&[u8]; symtab::NUM_STREAMS]>::try_from(inputs), usize::try_from(len)) else {
        return Err("bad parameters or inputs");
    };
    of_len(symtab::decode(params, streams, n), len)
}

fn of_len(out: Option<Vec<u8>>, len: u64) -> Result<Vec<u8>> {
    match out {
        Some(o) if o.len() as u64 == len => Ok(o),
        Some(_) => Err("output of another length"),
        None => Err("corrupt"),
    }
}
