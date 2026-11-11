//! The module `ac3-recons` natively on a whole input: the chunks decoded at once on threads.

use alloc::vec::Vec;

use aqr_walk::rc::{Decoder, SliceSource};

use crate::stream::{self, FRAME_MAX, RUN_PIECE};
use crate::{ModuleError as Error, Result, corrupt};

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
