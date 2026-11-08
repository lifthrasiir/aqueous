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

/// Pages of the windows of [`x86_part`]: of each of the inputs and of the output.
pub const X86_WINDOW: u32 = 1;

/// Bytes of the pieces in which [`x86_part`] takes each stream.
const PIECE: usize = 8 << 10;
/// Bytes of the section [`x86_part`] holds before it puts them out, and the bytes it holds back
/// (the sentinel cut off the end is up to 15).
const SPOOL: usize = 64 << 10;
const HOLD: usize = 16;
/// Bytes one step of the decoder appends at most (a padding record of 255 bytes, a jump table of
/// 256 entries of 8): the room besides [`SPOOL`].
const STEP: usize = 2048;

/// The memory plan of [`x86_part`] ([`aqr_api::stack`]): what it takes beyond its initial memory
/// (`initial` bytes) at most, whatever the lengths: a piece of each stream and the section held.
pub fn x86_part_plan(initial: u64) -> u64 {
    let mut m = aqr_api::stack::Sim::new(initial);
    for _ in 0..x86::NUM_STREAMS {
        m.take(PIECE as u64);
    }
    m.take((SPOOL + HOLD + STEP) as u64);
    m.plan()
}

/// The 13 streams of [`x86`], each read in order in pieces, with a byte of look-ahead.
struct Readers<'a> {
    r: [aqr_api::stream::Reader<'a>; x86::NUM_STREAMS],
    peeked: [Option<u8>; x86::NUM_STREAMS],
}

impl x86::Src for Readers<'_> {
    #[inline]
    fn read8(&mut self, st: usize) -> Option<u8> {
        self.peeked[st].take().or_else(|| self.r[st].byte())
    }

    fn read32(&mut self, st: usize) -> Option<u32> {
        let mut b = [0; 4];
        b.iter_mut().try_for_each(|x| Some(*x = self.read8(st)?))?;
        Some(u32::from_le_bytes(b))
    }

    fn read64(&mut self, st: usize) -> Option<u64> {
        let mut b = [0; 8];
        b.iter_mut().try_for_each(|x| Some(*x = self.read8(st)?))?;
        Some(u64::from_le_bytes(b))
    }

    #[inline]
    fn peek8(&mut self, st: usize) -> Option<u8> {
        if self.peeked[st].is_none() {
            self.peeked[st] = self.r[st].byte();
        }
        self.peeked[st]
    }

    #[inline]
    fn is_empty(&mut self, st: usize) -> bool {
        self.peeked[st].is_none() && self.r[st].done()
    }

    #[inline]
    fn copy<K: x86::Sink>(&mut self, n: usize, st: usize, out: &mut K) -> Option<()> {
        // (n is at most 255)
        let mut buf = [0u8; 255];
        let buf = buf.get_mut(..n)?;
        buf.iter_mut().try_for_each(|x| Some(*x = self.read8(st)?))?;
        out.extend_from_slice(buf);
        Some(())
    }
}

/// The section put out as it is built, but for its last [`HOLD`] bytes (the sentinel is cut off
/// them): once [`SPOOL`] bytes are held, those before them go to `sink`.
struct Spool<'a> {
    buf: Vec<u8>,
    sink: &'a mut dyn aqr_api::stream::Put,
    failed: bool,
    /// Bytes put out.
    flushed: usize,
}

impl Spool<'_> {
    fn put(&mut self, n: usize) {
        if !self.failed && self.sink.put(&self.buf[..n]).is_none() {
            self.failed = true;
        }
        self.buf.drain(..n);
        self.flushed += n;
    }

    #[inline]
    fn spill(&mut self) {
        if self.buf.len() >= SPOOL {
            self.put(self.buf.len() - HOLD);
        }
    }
}

impl x86::Sink for Spool<'_> {
    #[inline]
    fn push(&mut self, b: u8) {
        self.buf.push(b);
        self.spill();
    }

    #[inline]
    fn extend_from_slice(&mut self, d: &[u8]) {
        self.buf.extend_from_slice(d);
        self.spill();
    }

    fn fill(&mut self, n: usize, b: u8) {
        self.buf.resize(self.buf.len() + n, b);
        self.spill();
    }

    fn len(&self) -> usize {
        self.flushed + self.buf.len()
    }

    fn finish(&mut self, sentinel: usize) -> Option<()> {
        let n = self.buf.len().checked_sub(sentinel).filter(|_| sentinel <= HOLD && !self.failed)?;
        self.put(n);
        (!self.failed).then_some(())
    }
}

/// [`x86`] as a part module (its entry point, of `aqr_api::entry!`): its inputs taken and its
/// output put in order through windows ([`X86_WINDOW`]) in pieces, whatever the lengths. A
/// sentinel of more than 15 bytes is refused (FORMAT.md allows 0 to 15).
pub fn x86_part(params_len: u64, res_len: u64, lens: [u64; x86::NUM_STREAMS], size: u64) {
    aqr_api::finish(x86_part_run(params_len, res_len, lens, size));
}

fn x86_part_run(params_len: u64, res_len: u64, lens: [u64; x86::NUM_STREAMS], size: u64) -> Result<()> {
    use aqr_api::stream::{In, Pull, Written};
    use aqr_api::{Memory, Resource};
    if params_len < 10 || res_len != 1280 {
        return Err("bad parameters or resources");
    }
    let (mut p, mut table) = ([0u8; 10], [0u8; 1280]);
    aqr_api::Params::read(0, &mut p);
    Resource::<0>::read(0, &mut table);
    let mut ins = (
        In::<0>::new(lens[0]),
        In::<1>::new(lens[1]),
        In::<2>::new(lens[2]),
        In::<3>::new(lens[3]),
        In::<4>::new(lens[4]),
        In::<5>::new(lens[5]),
        In::<6>::new(lens[6]),
        In::<7>::new(lens[7]),
        In::<8>::new(lens[8]),
        In::<9>::new(lens[9]),
        In::<10>::new(lens[10]),
        In::<11>::new(lens[11]),
        In::<12>::new(lens[12]),
    );
    let srcs: [&mut dyn Pull; x86::NUM_STREAMS] = [
        &mut ins.0, &mut ins.1, &mut ins.2, &mut ins.3, &mut ins.4, &mut ins.5, &mut ins.6, &mut ins.7, &mut ins.8,
        &mut ins.9, &mut ins.10, &mut ins.11, &mut ins.12,
    ];
    let mut out = Written::new(size);
    x86_run(&p, &table, srcs, &mut out)?;
    (out.total() == size).then_some(()).ok_or("output of another length")
}

/// [`x86_part`] on whole inputs, natively: what it gives.
pub fn x86_streamed(params: &[u8], resources: &[&[u8]], inputs: &[&[u8]], len: u64) -> Result<Vec<u8>> {
    use aqr_api::stream::{Pull, Slice};
    let (Some(p), Ok(streams), &[table]) = (params.first_chunk::<10>(), <[&[u8]; x86::NUM_STREAMS]>::try_from(inputs), resources)
    else {
        return Err("bad parameters or inputs");
    };
    let mut slices = streams.map(Slice);
    let srcs: [&mut dyn Pull; x86::NUM_STREAMS] = slices.each_mut().map(|s| s as &mut dyn Pull);
    let mut out = Vec::new();
    x86_run(p, table, srcs, &mut out)?;
    (out.len() as u64 == len).then_some(out).ok_or("output of another length")
}

/// The decoding of [`x86`] on the streams `srcs`, put to `sink`; the length is for the caller to
/// check.
fn x86_run(
    p: &[u8; 10],
    table: &[u8],
    srcs: [&mut dyn aqr_api::stream::Pull; x86::NUM_STREAMS],
    sink: &mut dyn aqr_api::stream::Put,
) -> Result<()> {
    let opcodes = x86::Opcodes::from_resource(table).ok_or("bad resources")?;
    let origin = u64::from_le_bytes(*p.first_chunk().unwrap());
    let r = srcs.map(|s| aqr_api::stream::Reader::new(s, PIECE));
    let mut src = Readers { r, peeked: [None; x86::NUM_STREAMS] };
    let mut out = Spool { buf: Vec::with_capacity(SPOOL + HOLD + STEP), sink, failed: false, flushed: 0 };
    x86::decode_core(&mut src, &mut out, origin, p[8], p[9] != 0, &opcodes, |_, _, _| Some(())).ok_or("corrupt")
}
