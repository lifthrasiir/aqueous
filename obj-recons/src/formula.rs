//! obj-recons as a module of entry formulas (see recons-formula): the streams of the coded
//! blocks, one input slot per kind of stream across blocks, with an index of the blocks.
//! `FORMAT.md` specifies it (section "Formula module").
//!
//! The container's header (magic, version, length, hash), the block modes and the zstd of
//! streams and whole blocks are left out: the archive compresses the slots, and the hash is
//! the formula's business.

use crate::block::{self, NSTREAMS};
use crate::container::{self, AC_STREAMS, M_ANS, M_STORED, MAX_BLOCK, Options, Stored, Streams};
use crate::varint::{Rd, put_uv};
use crate::{Result, ans, corrupt};
use recons_formula::{Formula, Module, Node};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicUsize, Ordering};

pub const MODULE: Module = Module { name: "obj-recons", version: 1 };

/// Input slots: the index, the blocks stored whole, then one per stream of `block`.
const NINPUTS: usize = 2 + NSTREAMS;

/// A block as coded: stored whole, or its streams.
enum Blk<'a> {
    Whole(&'a [u8]),
    Coded(usize, Box<Streams>),
}

/// The formula rebuilding `input`, or None where it is empty or no block could be coded.
pub fn encode(input: &[u8], threads: usize) -> Option<Formula> {
    encode_with(input, &Options { threads, ..Options::default() })
}

/// `encode` with `opt` (`level` is not used).
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
    let mut streams = container::new_streams();
    let mut check = Vec::new();
    let mut blocks = Vec::new();
    let mut p = 0;
    while p < input.len() {
        let e = container::block_end(input, p, bsize);
        let b = &input[p..e];
        let (em, dm) = (enc.mark(), dec.mark());
        enc.encode(b, &mut streams);
        let mut ok = opt.force_whole >> (blocks.len() % 64) & 1 == 0;
        if ok && opt.verify {
            check.clear();
            ok = dec.decode(&streams, &mut check) && check == b;
        }
        if ok {
            blocks.push(Blk::Coded(b.len(), Box::new(std::mem::replace(&mut streams, container::new_streams()))));
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
        let next = AtomicUsize::new(0);
        let found = std::thread::scope(|sc| {
            let hs: Vec<_> = (0..opt.threads.min(blocks.len()))
                .map(|_| {
                    sc.spawn(|| {
                        let mut found = Vec::new();
                        loop {
                            let i = next.fetch_add(1, Ordering::Relaxed);
                            let Some(b) = blocks.get(i) else { break found };
                            found.push((i, ans_of(b)));
                        }
                    })
                })
                .collect();
            hs.into_iter().flat_map(|h| h.join().expect("rANS worker")).collect::<Vec<_>>()
        });
        for (i, a) in found {
            coded[i] = a;
        }
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

/// The module: the OBJ file of `len` bytes from `inputs` (see `FORMAT.md`), no parameters.
pub fn decode(params: &[u8], inputs: &[&[u8]], len: u64, threads: usize) -> Result<Vec<u8>> {
    let (&[], [index, whole, slots @ ..]) = (params, inputs) else { return Err(corrupt()) };
    if inputs.len() != NINPUTS {
        return Err(corrupt());
    }
    let mut ix = Rd::new(index);
    let (mut wp, mut sp) = (0usize, [0usize; NSTREAMS]);
    let mut next = |left: usize| -> Result<(usize, Stored)> {
        let v = ix.uv();
        let n = (v >> 1) as usize;
        if ix.overrun() || n == 0 || n > MAX_BLOCK || n > left {
            return Err(corrupt());
        }
        if v & 1 == 0 {
            let d = whole.get(wp..wp + n).ok_or_else(corrupt)?;
            wp += n;
            return Ok((n, Stored::Plain(d)));
        }
        let mut s = Box::new([(0, 0, &[][..]); NSTREAMS]);
        for (i, e) in s.iter_mut().enumerate() {
            let v = ix.uv();
            if v == 0 {
                continue;
            }
            let raw = (v >> 1) as usize;
            let m = if v & 1 == 1 { M_ANS } else { M_STORED };
            let c = if m == M_ANS { ix.uv() as usize } else { raw };
            if raw == 0 || raw > 8 * n + 64 || (m == M_ANS && !AC_STREAMS[i]) {
                return Err(corrupt());
            }
            *e = (raw, m, slots[i].get(sp[i]..sp[i].saturating_add(c)).ok_or_else(corrupt)?);
            sp[i] += c;
        }
        if ix.overrun() {
            return Err(corrupt());
        }
        Ok((n, Stored::Coded(s)))
    };
    let total = inputs.iter().map(|i| i.len()).sum::<usize>();
    let mut out = Vec::with_capacity((len as usize).min(total.saturating_mul(64)));
    container::decode_blocks(&mut next, len as usize, &mut out, threads, false)?;
    if !ix.done() || wp != whole.len() || sp.iter().zip(slots).any(|(&p, s)| p != s.len()) {
        return Err(corrupt());
    }
    Ok(out)
}
