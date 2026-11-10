//! aqr-prores as a module of entry formulas (see aqr-formula): a QuickTime or MXF file whose
//! ProRes frames are coded in chunks, each chunk's span (its frames and the bytes between them)
//! the output of an application. `FORMAT.md` specifies the module.

use std::ops::Range;

use aqr_formula::{Formula, Kind, Module, Node};

use crate::codec;
use crate::frame::Frame;
use crate::module;

pub use crate::module::decode;

pub const MODULE: Module = Module::new("prores-recons", 3).with_task_plan(memory);

/// Frame bytes of a chunk, about: the encoder holds a chunk whole, and its stripes are about 4 MiB
/// each (`codec::STRIPE_BYTES`), 16 of them. (The decoder holds a few frames, not a chunk.)
pub const CHUNK: usize = 64 << 20;

/// The class of the stream holding the frames' coefficients, which tells a ProRes formula.
const CODED: &str = "prores/coded";
/// The class of the stream holding the frames' headers and the like.
const SIDE: &str = "prores/side";

/// Bytes between frames past which a chunk ends (they stay out of the applications).
const GAP: usize = 4 << 20;
/// How far into a file its first frame is looked for.
const FIRST: usize = 64 << 20;

/// The memory of the module: its plan ([`crate::module::plan`]), the messages its channels may hold
/// with it.
fn memory(u: &aqr_formula::Usage<'_>) -> Option<aqr_formula::Plan> {
    let &[_] = u.outputs else { return None };
    let ((instance, gang), _) = module::plan(u.params, u.initial())?;
    Some(aqr_formula::Plan::one(instance, gang))
}

/// Whether `d` starts as a QuickTime or MXF file and holds a ProRes frame near its start.
pub fn starts(d: &[u8]) -> bool {
    const ATOMS: [&[u8; 4]; 7] = [b"ftyp", b"moov", b"mdat", b"wide", b"free", b"skip", b"pnot"];
    let qt = d.len() >= 8 && ATOMS.iter().any(|a| &d[4..8] == *a);
    let mxf = d.starts_with(&[0x06, 0x0e, 0x2b, 0x34, 0x02]);
    if !qt && !mxf {
        return false;
    }
    let head = &d[..d.len().min(FIRST)];
    let mut at = 4;
    while let Some(i) = memchr::memmem::find(&head[at..], b"icpf").map(|i| i + at) {
        if Frame::parse(&d[i - 4..]).is_ok() {
            return true;
        }
        at = i + 1;
    }
    false
}

/// The extent of such a file: all of it.
pub fn extent(d: &[u8]) -> Option<usize> {
    starts(d).then_some(d.len())
}

/// The size of the frame at `at` in `d`.
fn size(d: &[u8], at: usize) -> usize {
    u32::from_be_bytes(d[at..at + 4].try_into().unwrap()) as usize
}

/// The frames (by their indices in `frames`, offsets in `d` in order) of each chunk.
fn chunks(d: &[u8], frames: &[usize], chunk: usize) -> Vec<Range<usize>> {
    let end = |i: usize| frames[i] + size(d, frames[i]);
    let mut out = Vec::new();
    let (mut first, mut bytes) = (0, 0);
    for (i, &at) in frames.iter().enumerate() {
        if i > first && (bytes >= chunk || at - end(i - 1) > GAP) {
            out.push(first..i);
            (first, bytes) = (i, 0);
        }
        bytes += size(d, at);
    }
    if first < frames.len() {
        out.push(first..frames.len());
    }
    out
}

/// The formula of `input`, a file holding ProRes frames, on `threads` threads (the formula does
/// not depend on it); None where there are none, or coding them saves nothing.
pub fn encode(input: &[u8], threads: usize) -> Option<Formula> {
    encode_with(input, threads, CHUNK, None)
}

/// [`encode`] with chunks of about `chunk` bytes of frames, and `stripes` stripes in each where
/// given (else by size, `codec::STRIPE_BYTES` each).
pub fn encode_with(input: &[u8], threads: usize, chunk: usize, stripes: Option<usize>) -> Option<Formula> {
    let frames = crate::find(input);
    if frames.is_empty() {
        return None;
    }
    let groups = chunks(input, &frames, chunk);
    let coded: Vec<(Range<usize>, codec::Coded)> = aqr_common::pool::par_map(groups.len(), threads, |g| {
        let fs = &frames[groups[g].clone()];
        let last = fs[fs.len() - 1];
        let span = fs[0]..last + size(input, last);
        let ranges: Vec<_> = fs.iter().map(|&a| a - span.start..a - span.start + size(input, a)).collect();
        let c = match stripes {
            Some(k) => codec::encode_with(&input[span.clone()], &ranges, k),
            None => codec::encode(&input[span.clone()], &ranges),
        };
        (span, c)
    });
    let frame_bytes: usize = frames.iter().map(|&a| size(input, a)).sum();
    let coded_bytes: usize = coded.iter().map(|c| c.1.coded.len() + c.1.side.len()).sum();
    // (aqr-compress weighs the formula against the file; this only spares it the hopeless)
    if coded_bytes >= frame_bytes {
        return None;
    }
    // The bytes outside the frames in order, and each application's inputs in the streams.
    let (mut rest, mut all, mut sides) = (Vec::new(), Vec::with_capacity(coded_bytes), Vec::new());
    let mut parts = Vec::with_capacity(coded.len());
    let mut at = 0;
    for (span, c) in &coded {
        let before = (rest.len(), span.start - at);
        rest.extend_from_slice(&input[at..span.start]);
        let gaps = (rest.len(), c.gaps.len());
        rest.extend_from_slice(&c.gaps);
        let side = (sides.len(), c.side.len());
        sides.extend_from_slice(&c.side);
        parts.push((before, side, (all.len(), c.coded.len()), gaps, span.len(), c.limits.clone()));
        all.extend_from_slice(&c.coded);
        at = span.end;
    }
    let tail = (rest.len(), input.len() - at);
    rest.extend_from_slice(&input[at..]);
    let mut f = Formula::default();
    let Node::Slice { stream: ss, .. } = f.push_as(SIDE, Kind::Compressed, sides) else { unreachable!() };
    let Node::Slice { stream: cs, .. } = f.push_as(CODED, Kind::Compressed, all) else { unreachable!() };
    let Node::Slice { stream: rs, .. } = f.push("prores/rest", rest) else { unreachable!() };
    let slice = |s: usize, (start, n): (usize, usize)| Node::slice(s, start as u64, n as u64);
    let mut root = Vec::with_capacity(2 * parts.len() + 1);
    for (before, side, coded, gaps, span, limits) in parts {
        root.push(slice(rs, before));
        let inputs = vec![slice(ss, side), slice(cs, coded), slice(rs, gaps)];
        root.push(Node::apply(MODULE, module::params(&limits), inputs, span as u64));
    }
    root.push(slice(rs, tail));
    f.root = Node::concat(root);
    Some(f)
}

/// Whether formula `f` is one of [`encode`]'s, or holds one (a scan's of a file, say).
pub fn is(f: &Formula) -> bool {
    f.streams.iter().any(|s| s.class == CODED)
}
