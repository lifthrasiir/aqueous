//! The code of the module (see `formula`, and `FORMAT.md`), which needs no aqr-formula, so that
//! aqr-modules builds it into its Wasm module.

use crate::block::NSTREAMS;
use crate::codec::{self, AC_STREAMS, M_ANS, M_STORED, MAX_BLOCK, Sizes, Stored};
use crate::varint::Rd;
use crate::{Result, corrupt};

/// Input slots: the index, the blocks stored whole, then one per stream of `block`.
pub(crate) const NINPUTS: usize = 2 + NSTREAMS;

/// What index `index` tells of the coded blocks ([`Sizes`]).
fn sizes(index: &[u8]) -> Result<Sizes> {
    let mut s = Sizes { longest: [0; NSTREAMS], templates: 0, coded: false, ans: false };
    let mut ix = Rd::new(index);
    while !ix.done() {
        if ix.uv() & 1 == 1 {
            s.coded = true;
            for (i, l) in s.longest.iter_mut().enumerate() {
                let v = ix.uv();
                let raw = usize::try_from(v >> 1).map_err(|_| corrupt())?;
                *l = (*l).max(raw);
                if i == crate::block::S_TPL {
                    s.templates = s.templates.saturating_add(raw);
                }
                if v & 1 == 1 {
                    s.ans = true;
                    ix.uv();
                }
            }
        }
        if ix.overrun() {
            return Err(corrupt());
        }
    }
    Ok(s)
}

/// The memory plan of the module ([`aqr_api::stack`]): what it takes beyond its initial memory
/// at most, of inputs of `inputs` bytes, the index `index` and a file of `len` bytes (`None`
/// where they are not so): what `aqr_api::whole` copies, the file, and what decoding takes
/// (`Sizes::plan`).
pub fn plan(inputs: &[u64], index: &[u8], len: u64) -> Option<u64> {
    use aqr_api::stack::{block, whole};
    let s = sizes(index).ok()?;
    Some(whole(0, &[], inputs) + block(len) + s.plan(usize::try_from(len).ok()?))
}

/// The module: the OBJ file of `len` bytes from `inputs` (see `FORMAT.md`), no parameters.
pub fn decode(params: &[u8], inputs: &[&[u8]], len: u64, threads: usize) -> Result<Vec<u8>> {
    let (&[], [index, whole, slots @ ..]) = (params, inputs) else { return Err(corrupt()) };
    if inputs.len() != NINPUTS {
        return Err(corrupt());
    }
    let sizes = sizes(index)?;
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
            return Ok((n, Stored::Whole(d)));
        }
        let mut s = [(0, 0, &[][..]); NSTREAMS];
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
    let len = usize::try_from(len).map_err(|_| corrupt())?;
    // (natively, no more than a corrupt length could ask for; in Wasm its plan)
    let total = inputs.iter().map(|i| i.len()).sum::<usize>();
    let cap = if cfg!(target_arch = "wasm32") { len } else { len.min(total.saturating_mul(64)) };
    let mut out = Vec::with_capacity(cap);
    codec::decode_blocks(&mut next, len, &sizes, &mut out, threads)?;
    if !ix.done() || wp != whole.len() || sp.iter().zip(slots).any(|(&p, s)| p != s.len()) {
        return Err(corrupt());
    }
    Ok(out)
}
