//! The code of the module `bz2-recons` (see `formula`, and `FORMAT.md`), which needs no
//! aqr-formula, so that aqr-modules builds it into its Wasm module.

use aqr_common::varint;

use crate::compress;
use crate::{Error, Result, corrupt};

/// The parameters of one stream: its level, bytes of data, and the blocks whose `origPtr`
/// differs from the one compressing gives (index, `origPtr`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StreamParams {
    pub level: u8,
    pub len: u64,
    pub orig_ptrs: Vec<(usize, u32)>,
}

pub fn put_params(out: &mut Vec<u8>, streams: &[StreamParams]) {
    varint::put(out, streams.len() as u64);
    for s in streams {
        out.push(s.level);
        varint::put(out, s.len);
        varint::put(out, s.orig_ptrs.len() as u64);
        let mut next = 0;
        for &(b, p) in &s.orig_ptrs {
            varint::put(out, (b - next) as u64);
            varint::put(out, p as u64);
            next = b + 1;
        }
    }
}

pub fn get_params(p: &[u8]) -> Result<Vec<StreamParams>> {
    let mut at = 0;
    let mut get = || varint::get(p, &mut at).map_err(|_| corrupt());
    let n = get()?;
    if n == 0 || n > p.len() as u64 {
        return Err(corrupt());
    }
    let mut streams = Vec::with_capacity(n as usize);
    drop(get);
    for _ in 0..n {
        let level = *p.get(at).ok_or_else(corrupt)?;
        at += 1;
        if !(1..=9).contains(&level) {
            return Err(corrupt());
        }
        let mut get = || varint::get(p, &mut at).map_err(|_| corrupt());
        let len = get()?;
        let k = get()?;
        if k > p.len() as u64 {
            return Err(corrupt());
        }
        let mut orig_ptrs = Vec::with_capacity(k as usize);
        let mut next = 0u64;
        for _ in 0..k {
            let b = next.checked_add(get()?).filter(|&b| b < u32::MAX as u64).ok_or_else(corrupt)?;
            let o = get()?;
            if o >= 1 << 24 {
                return Err(corrupt());
            }
            orig_ptrs.push((b as usize, o as u32));
            next = b + 1;
        }
        streams.push(StreamParams { level, len, orig_ptrs });
    }
    if at != p.len() {
        return Err(corrupt());
    }
    Ok(streams)
}

/// The streams of `data`, one after another, compressed as `streams` says.
pub fn compress(data: &[u8], streams: &[StreamParams], threads: usize) -> Result<Vec<u8>> {
    let total = streams.iter().try_fold(0u64, |a, s| a.checked_add(s.len));
    if total != Some(data.len() as u64) {
        return Err(Error("data of another length"));
    }
    let mut out = Vec::new();
    let mut at = 0;
    for s in streams {
        let d = &data[at..at + s.len as usize];
        at += s.len as usize;
        let mut blocks = compress::blocks(d, s.level, threads);
        for &(b, p) in &s.orig_ptrs {
            let blk = blocks.get_mut(b).ok_or_else(corrupt)?;
            if p as usize >= blk.syms {
                return Err(corrupt());
            }
            blk.set_orig_ptr(p);
        }
        out.extend_from_slice(&compress::stream(s.level, &blocks));
    }
    Ok(out)
}

/// The module: the bzip2 streams of `len` bytes that the parameters make of the single input
/// (see `FORMAT.md`).
pub fn decode(params: &[u8], inputs: &[&[u8]], len: u64, threads: usize) -> Result<Vec<u8>> {
    let &[data] = inputs else { return Err(Error("one input wanted")) };
    let streams = get_params(params)?;
    let out = compress(data, &streams, threads)?;
    if out.len() as u64 != len {
        return Err(Error("output of another length"));
    }
    Ok(out)
}
