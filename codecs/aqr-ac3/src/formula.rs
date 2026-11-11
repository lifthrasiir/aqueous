//! aqr-ac3 as a module of entry formulas (see aqr-formula): an AC-3 byte stream from its coded form. `FORMAT.md`
//! specifies it.
use std::vec::Vec;

use aqr_formula::Module;
use aqr_walk::rc::{Decoder, Encoder, SliceSource};

use crate::stream::{self, FRAME_MAX, RUN_PIECE};
use crate::{ModuleError as Error, Result, corrupt};

/// The class of the coded stream.
pub const CODED: &str = "ac3/frames";

/// The module `ac3-recons`: no params; the input the coded stream.
pub const MODULE: Module = Module::new("ac3-recons", 1).with_plan(memory);

fn memory(u: &aqr_formula::Usage<'_>) -> Option<u64> {
    let &[_] = u.inputs else { return None };
    Some(crate::module::plan(u.initial()))
}

fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push(v as u8 | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

/// The coded form of the byte stream `data` (chunks coded at once on `threads`), if it comes to less than 97% of it.
pub fn encode_with(data: &[u8], threads: usize) -> Option<Vec<u8>> {
    let out = encode_always(data, threads);
    (out.len() as f64 <= 0.97 * data.len() as f64).then_some(out)
}

/// The coded form of the byte stream `data` whatever its size (for tests).
pub fn encode_always(data: &[u8], threads: usize) -> Vec<u8> {
    let recs = stream::records(data);
    let chunks = stream::chunks(&recs);
    let parts = aqr_common::pool::par_map(chunks.len(), threads, |i| {
        let rs = &recs[chunks[i].clone()];
        let bytes = rs.last().unwrap().end - rs[0].start;
        let mut enc = Encoder::new();
        stream::encode_chunk(&mut enc, data, rs);
        (bytes, enc.finish())
    });
    let mut out = Vec::new();
    put_varint(&mut out, parts.len() as u64);
    for (bytes, coded) in &parts {
        put_varint(&mut out, *bytes as u64);
        put_varint(&mut out, coded.len() as u64);
    }
    for (_, coded) in &parts {
        out.extend_from_slice(coded);
    }
    out
}

/// [`encode_with`] on the threads of the machine.
pub fn encode(data: &[u8]) -> Option<Vec<u8>> {
    encode_with(data, std::thread::available_parallelism().map_or(1, |n| n.get()))
}

fn read_varint(data: &[u8], pos: &mut usize) -> Result<u64> {
    let (mut v, mut shift) = (0u64, 0);
    loop {
        let b = *data.get(*pos).ok_or_else(corrupt)?;
        *pos += 1;
        if shift > 56 {
            return Err(corrupt());
        }
        v |= ((b & 0x7f) as u64) << shift;
        if b & 0x80 == 0 {
            return Ok(v);
        }
        shift += 7;
    }
}

/// The module `ac3-recons` on a whole input, the chunks decoded at once on `threads`.
pub fn decode(_params: &[u8], inputs: &[&[u8]], len: u64, threads: usize) -> Result<Vec<u8>> {
    let &[coded] = inputs else { return Err(corrupt()) };
    let mut pos = 0;
    let n = read_varint(coded, &mut pos)?;
    if n > 1 << 24 {
        return Err(corrupt());
    }
    let mut table = Vec::new();
    for _ in 0..n {
        let a = read_varint(coded, &mut pos)?;
        let b = read_varint(coded, &mut pos)?;
        table.push((a, b));
    }
    let mut ranges = Vec::new();
    let (mut at, mut total) = (pos as u64, 0u64);
    for &(a, b) in &table {
        ranges.push((at as usize, (at + b) as usize, a as usize));
        at = at.checked_add(b).ok_or_else(corrupt)?;
        total = total.checked_add(a).ok_or_else(corrupt)?;
    }
    if total != len || at > coded.len() as u64 {
        return Err(corrupt());
    }
    let parts = aqr_common::pool::par_map(ranges.len(), threads, |i| -> Result<Vec<u8>> {
        let (from, to, bytes) = ranges[i];
        let mut rc = Decoder::new(SliceSource::new(&coded[from..to]));
        let mut sd = stream::Decoder::new();
        let mut out = Vec::with_capacity(bytes);
        let mut buf = Vec::with_capacity(FRAME_MAX.max(RUN_PIECE) + 16);
        while out.len() < bytes {
            buf = sd.next(&mut rc, buf).map_err(|_| corrupt())?;
            out.extend_from_slice(&buf);
        }
        if out.len() != bytes {
            return Err(Error("output of another length"));
        }
        Ok(out)
    });
    let mut out = Vec::with_capacity(len as usize);
    for p in parts {
        out.extend_from_slice(&p?);
    }
    Ok(out)
}
