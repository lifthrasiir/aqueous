//! The module `ac3-recons` natively on a whole input: the chunks decoded at once on threads.

use alloc::vec::Vec;

use aqr_walk::chunks;
use aqr_walk::rc::{Decoder, SliceSource};

use crate::module::RECORD;
use crate::stream;
use crate::{ModuleError as Error, Result, corrupt};

/// The most bytes reserved for a chunk's output before they are decoded (a declared size may be anything).
const OUT_RESERVE: usize = 256 << 20;

/// The module `ac3-recons` on a whole input, the chunks decoded at once on `threads`.
pub fn decode(_params: &[u8], inputs: &[&[u8]], len: u64, threads: usize) -> Result<Vec<u8>> {
    let &[coded] = inputs else { return Err(corrupt()) };
    let mut pos = 0;
    let table = chunks::read_at(coded, &mut pos).map_err(|_| corrupt())?;
    let mut ranges = Vec::new();
    let (mut at, mut total) = (pos as u64, 0u64);
    for &(a, b) in &table {
        let end = at.checked_add(b).ok_or_else(corrupt)?;
        ranges.push((at as usize, end as usize, a as usize));
        at = end;
        total = total.checked_add(a).ok_or_else(corrupt)?;
    }
    if total != len || at > coded.len() as u64 {
        return Err(corrupt());
    }
    let parts = aqr_common::pool::par_map(ranges.len(), threads, |i| -> Result<Vec<u8>> {
        let (from, to, bytes) = ranges[i];
        let mut rc = Decoder::new(SliceSource::new(&coded[from..to]));
        let mut sd = stream::Decoder::new();
        let mut out = Vec::with_capacity(bytes.min(OUT_RESERVE));
        let mut buf = Vec::with_capacity(RECORD);
        while out.len() < bytes {
            buf = sd.next(&mut rc, buf).map_err(|_| corrupt())?;
            out.extend_from_slice(&buf);
        }
        if out.len() != bytes {
            return Err(Error("output of another length"));
        }
        Ok(out)
    });
    let parts = parts.into_iter().collect::<Result<Vec<_>>>()?;
    let mut out = Vec::with_capacity(parts.iter().map(Vec::len).sum());
    for p in &parts {
        out.extend_from_slice(p);
    }
    Ok(out)
}
