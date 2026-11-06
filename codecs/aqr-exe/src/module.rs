//! The code of the modules of aqr-exe (`exe-recons/x86`, `exe-recons/x86-remap`,
//! `exe-recons/arm64`, `exe-recons/arm64-remap`, `exe-recons/arm64-delta`,
//! `exe-recons/macho-codesign`, `exe-recons/cil`, `exe-recons/pe-reloc` and
//! `exe-recons/macho-symtab`, as aqr-decompress names them): the parameters and the inputs
//! checked, then the section or table decoded. aqr-modules builds it into their Wasm modules.

use alloc::vec::Vec;

use crate::arch::{arm64, cil, x86};
use crate::{reloc, symtab};

pub type Result<T> = core::result::Result<T, &'static str>;

/// The memory plan ([`aqr_api::stack`]) of [`arm64`] and [`reloc`]: what each takes beyond its
/// initial memory at most, of params, resources and inputs of these lengths and an output of
/// `len`: what `aqr_api::whole` copies, and the output.
pub fn plan(params: u64, resources: &[u64], inputs: &[u64], len: u64) -> u64 {
    aqr_api::stack::whole(params, resources, inputs) + aqr_api::stack::block(len)
}

/// The memory plan of [`x86`] and [`x86_remap`], as [`plan`]'s of params `params`: the output
/// with the sentinel after it (`None` where the params are too short).
pub fn x86_plan(params: &[u8], resources: &[u64], inputs: &[u64], len: u64) -> Option<u64> {
    let sentinel = *params.get(8)? as u64;
    Some(plan(params.len() as u64, resources, inputs, len.checked_add(sentinel)?))
}

/// The memory plan of [`x86_remap`], as [`x86_plan`]'s of the streams from `x86::remap::CALLF` on
/// `flags` (the three flag streams and the map).
pub fn x86_remap_plan(params: &[u8], resources: &[u64], inputs: &[u64], flags: [&[u8]; 4], len: u64) -> Option<u64> {
    let room = len.checked_add(*params.get(8)? as u64)?;
    let whole = aqr_api::stack::whole(params.len() as u64, resources, inputs);
    Some(whole + x86::remap::decode_plan(flags, room))
}

/// The memory plan of [`arm64_remap`], as [`plan`]'s of the streams from
/// `arm64::remap::BLF` on `flags` (the two flag streams and the two maps).
pub fn arm64_remap_plan(params: u64, inputs: &[u64], flags: [&[u8]; 4], len: u64) -> u64 {
    aqr_api::stack::whole(params, &[], inputs) + arm64::remap::decode_plan(flags, len)
}

/// The memory plan of [`arm64_delta`], as [`plan`]'s: the output and the table of predictions.
pub fn arm64_delta_plan(inputs: &[u64], len: u64) -> u64 {
    use aqr_api::stack::block;
    aqr_api::stack::whole(0, &[], inputs) + block(len) + block(8 << arm64::delta::TABLE_BITS)
}

/// The memory plan of [`macho_codesign`], as [`plan`]'s of the signatures `sigs` (`None` where
/// the params are not of them).
pub fn macho_codesign_plan(params: &[u8], inputs: &[u64], sigs: &[u8], len: u64) -> Option<u64> {
    Some(aqr_api::stack::whole(params.len() as u64, &[], inputs) + crate::codesign::decode_plan(params, sigs, len)?)
}

/// The memory plan of [`cil`], as [`plan`]'s of the streams `META`, `BODYPOS` and `TOKT`
/// (`None` where the metadata root does not parse).
pub fn cil_plan(params: u64, inputs: &[u64], streams: [&[u8]; 3], len: u64) -> Option<u64> {
    let [meta, bodypos, tokt] = streams;
    Some(aqr_api::stack::whole(params, &[], inputs) + cil::decode_plan(meta, bodypos, tokt, len)?)
}

/// The memory plan of [`macho_symtab`], as [`plan`]'s of the streams of the entries' types and
/// names.
pub fn macho_symtab_plan(params: u64, inputs: &[u64], types: &[u8], names: &[u8], len: u64) -> u64 {
    aqr_api::stack::whole(params, &[], inputs) + symtab::decode_plan(types, names, len)
}

/// The output's room of [`x86`] and [`x86_remap`] (the sentinel decoded after it, then cut).
fn x86_room(len: u64, sentinel: u8) -> usize {
    usize::try_from(len).ok().and_then(|n| n.checked_add(sentinel as usize)).unwrap_or(usize::MAX)
}

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
    let cap = x86_room(len, p[8]);
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
    let cap = x86_room(len, p[8]);
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

/// A stream of ARM64 code against that of another version (the stream, not the section: an
/// input of [`arm64_remap`]'s). No parameters; inputs: the other version's stream, then the
/// streams of [`arm64::delta::STREAM_NAMES`].
pub fn arm64_delta(params: &[u8], inputs: &[&[u8]], len: u64) -> Result<Vec<u8>> {
    let (true, [reference, streams @ ..], Ok(n)) = (params.is_empty(), inputs, usize::try_from(len)) else {
        return Err("bad parameters or inputs");
    };
    let Ok(streams) = <[&[u8]; arm64::delta::NUM_STREAMS]>::try_from(streams) else {
        return Err("bad parameters or inputs");
    };
    of_len(arm64::delta::decode(reference, streams, n), len)
}

/// A Mach-O file with its code signatures' code slots derived. Params and inputs (the body,
/// then the signatures): as [`crate::codesign`] tells.
pub fn macho_codesign(params: &[u8], inputs: &[&[u8]], len: u64) -> Result<Vec<u8>> {
    let (&[body, sigs], Ok(n)) = (inputs, usize::try_from(len)) else {
        return Err("bad parameters or inputs");
    };
    of_len(crate::codesign::decode(params, body, sigs, n), len)
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
