//! aqr-obj as a module of entry formulas (see aqr-formula): the streams of the coded
//! blocks, one input slot per kind of stream across blocks, with an index of the blocks.
//! `FORMAT.md` specifies it. The archive compresses the slots, and checksums the output.

use crate::ans;
use crate::block::{self, NSTREAMS};
use crate::codec::{self, AC_STREAMS, MAX_BLOCK, Options, Streams};
use crate::varint::put_uv;
use aqr_formula::{Formula, Module, Node};
use std::panic::{AssertUnwindSafe, catch_unwind};

pub use crate::module::decode;

pub const MODULE: Module = Module::new("obj-recons", 1).with_memory(memory);

/// The memory of the module, a block at a time: the inputs and the file (reserved for 64 times
/// the inputs at most, doubling past that, each block decoded onto it); each stream as long as
/// its longest in a block, as the index tells (twice, the old held as one grows); and the decoder's
/// state: three rings of recent lines and the corners, reserved for the lines of the file at most
/// (half the file) up to 2^16 of 40 bytes each; and, half again as its vectors grow, the templates' bytes, of
/// the template stream at most (in all blocks) up to 4 MiB, a vector twice that at most, and
/// their keys (8 bytes each, a byte each at least).
fn memory(u: &aqr_formula::Usage<'_>) -> Option<u64> {
    let index = u.content(0)?;
    let (mut ix, mut streams) = (crate::varint::Rd::new(index), [0u64; NSTREAMS]);
    let mut tpl = 0u64;
    while !ix.done() {
        if ix.uv() & 1 == 1 {
            for (i, s) in streams.iter_mut().enumerate() {
                let v = ix.uv();
                *s = (*s).max(v >> 1);
                if i == block::S_TPL {
                    tpl += v >> 1;
                }
                if v & 1 == 1 {
                    ix.uv();
                }
            }
        }
        if ix.overrun() {
            return None;
        }
    }
    let m = u.output();
    let reserved = m.min(64 * u.copied());
    let growth = if reserved < m { 2 * m } else { 0 };
    let lines = (m / 2).min(1 << 16);
    let arena = tpl.min(4 << 20);
    let state = 4 * lines * 40 + 3 * (2 * arena + 8 * arena.min(1 << 16)) / 2;
    Some(u.whole(growth + 2 * streams.iter().sum::<u64>() + state, 1 + NSTREAMS as u64 + 7))
}

/// Statement keywords of OBJ files (the specification's, those of its free-form geometry
/// included), the comment aside.
const STATEMENTS: [&[u8]; 37] = [
    b"v",
    b"vt",
    b"vn",
    b"vp",
    b"f",
    b"l",
    b"p",
    b"g",
    b"o",
    b"s",
    b"mg",
    b"usemtl",
    b"mtllib",
    b"usemap",
    b"maps",
    b"lod",
    b"bevel",
    b"c_interp",
    b"d_interp",
    b"shadow_obj",
    b"trace_obj",
    b"cstype",
    b"deg",
    b"bmat",
    b"step",
    b"curv",
    b"curv2",
    b"surf",
    b"parm",
    b"trim",
    b"hole",
    b"scrv",
    b"sp",
    b"end",
    b"con",
    b"call",
    b"csh",
];

/// Whether `d` starts like a Wavefront OBJ file: in its first 64 KiB (its whole lines), text
/// only, of statements, comments and blank lines, but one line in 32 at most, with three vertices
/// (`v` and at least three numbers) or more. An object file of the same extension (COFF, OMF)
/// is binary, and does not.
pub fn starts(d: &[u8]) -> bool {
    let head = &d[..d.len().min(64 << 10)];
    let head = match head.len() < d.len() {
        true => &head[..head.iter().rposition(|&b| b == b'\n').map_or(0, |p| p + 1)],
        false => head,
    };
    if head.iter().any(|&b| b < 0x20 && !matches!(b, b'\t' | b'\n' | b'\r') || b == 0x7f) {
        return false;
    }
    let (mut known, mut other, mut verts) = (0usize, 0usize, 0usize);
    for line in head.split(|&b| b == b'\n') {
        let mut words = line.split(|&b| matches!(b, b' ' | b'\t' | b'\r')).filter(|w| !w.is_empty());
        let Some(kw) = words.next() else { continue };
        if kw.starts_with(b"#") {
            continue;
        }
        if !STATEMENTS.contains(&kw) {
            other += 1;
            continue;
        }
        known += 1;
        let number = |w: &[u8]| std::str::from_utf8(w).is_ok_and(|w| w.parse::<f64>().is_ok());
        if kw == b"v" && words.take(3).filter(|w| number(w)).count() == 3 {
            verts += 1;
        }
    }
    verts >= 3 && other * 32 <= known
}

/// A block as coded: stored whole, or its streams.
enum Blk<'a> {
    Whole(&'a [u8]),
    Coded(usize, Box<Streams>),
}

/// The formula rebuilding `input`, or None where it is empty or no block could be coded.
pub fn encode(input: &[u8], threads: usize) -> Option<Formula> {
    encode_with(input, &Options { threads, ..Options::default() })
}

/// `encode` with `opt`.
pub fn encode_with(input: &[u8], opt: &Options) -> Option<Formula> {
    if input.is_empty() {
        return None;
    }
    catch_unwind(AssertUnwindSafe(|| encode_blocks(input, opt))).ok().flatten()
}

fn encode_blocks(input: &[u8], opt: &Options) -> Option<Formula> {
    let bsize = opt.block.clamp(1 << 10, MAX_BLOCK);
    let mut enc = block::Encoder::default();
    let mut dec = block::Decoder::default();
    let mut streams = codec::new_streams();
    let mut check = Vec::new();
    let mut blocks = Vec::new();
    let mut p = 0;
    while p < input.len() {
        let e = codec::block_end(input, p, bsize);
        let b = &input[p..e];
        let (em, dm) = (enc.mark(), dec.mark());
        enc.encode(b, &mut streams);
        let mut ok = opt.force_whole >> (blocks.len() % 64) & 1 == 0;
        if ok && opt.verify {
            check.clear();
            ok = dec.decode(&streams, &mut check) && check == b;
        }
        if ok {
            blocks.push(Blk::Coded(b.len(), Box::new(std::mem::replace(&mut streams, codec::new_streams()))));
        } else {
            enc.reset(&em);
            dec.reset(&dm);
            blocks.push(Blk::Whole(b));
        }
        p = e;
    }
    if blocks.iter().all(|b| matches!(b, Blk::Whole(_))) {
        return None;
    }

    // Per stream of a coded block, its rANS coding where that is smaller than storing it.
    let ans_of = |b: &Blk| -> Vec<Option<Vec<u8>>> {
        let Blk::Coded(_, streams) = b else { return Vec::new() };
        let mut ac = Vec::new();
        streams
            .iter()
            .enumerate()
            .map(|(i, s)| {
                ac.clear();
                (AC_STREAMS[i] && s.len() > 64 && ans::encode(s, &mut ac).is_some() && ac.len() < s.len())
                    .then(|| ac.clone())
            })
            .collect()
    };
    let mut coded: Vec<Vec<Option<Vec<u8>>>> = vec![Vec::new(); blocks.len()];
    if opt.threads > 1 && blocks.len() > 1 {
        coded = aqr_common::pool::par_map(blocks.len(), opt.threads, |i| ans_of(&blocks[i]));
    } else {
        for (c, b) in coded.iter_mut().zip(&blocks) {
            *c = ans_of(b);
        }
    }

    let mut index = Vec::new();
    let mut whole = Vec::new();
    let mut slots: Vec<Vec<u8>> = vec![Vec::new(); NSTREAMS];
    for (b, a) in blocks.iter().zip(&coded) {
        match b {
            Blk::Whole(d) => {
                put_uv(&mut index, (d.len() as u64) << 1);
                whole.extend_from_slice(d);
            }
            Blk::Coded(n, streams) => {
                put_uv(&mut index, (*n as u64) << 1 | 1);
                for (i, s) in streams.iter().enumerate() {
                    let raw = (s.len() as u64) << 1;
                    match &a[i] {
                        _ if s.is_empty() => put_uv(&mut index, 0),
                        Some(d) => {
                            put_uv(&mut index, raw | 1);
                            put_uv(&mut index, d.len() as u64);
                            slots[i].extend_from_slice(d);
                        }
                        None => {
                            put_uv(&mut index, raw);
                            slots[i].extend_from_slice(s);
                        }
                    }
                }
            }
        }
    }
    let mut f = Formula::default();
    let mut inputs = vec![f.push("obj/index", index), f.push("obj/whole", whole)];
    for (name, d) in block::STREAM_NAMES.iter().zip(slots) {
        inputs.push(f.push(format!("obj/{name}"), d));
    }
    f.root = Node::apply(MODULE, Vec::new(), inputs, input.len() as u64);
    Some(f)
}
