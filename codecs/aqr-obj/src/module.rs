//! The code of the module (see `formula`, and `FORMAT.md`), which needs no aqr-formula, so that
//! aqr-modules builds it into its Wasm module.

use crate::block::NSTREAMS;
use crate::codec::{self, AC_STREAMS, M_ANS, M_STORED, MAX_BLOCK, Stored};
use crate::varint::Rd;
use crate::{Result, corrupt};

/// Input slots: the index, the blocks stored whole, then one per stream of `block`.
pub(crate) const NINPUTS: usize = 2 + NSTREAMS;

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
            return Ok((n, Stored::Whole(d)));
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
    codec::decode_blocks(&mut next, len as usize, &mut out, threads)?;
    if !ix.done() || wp != whole.len() || sp.iter().zip(slots).any(|(&p, s)| p != s.len()) {
        return Err(corrupt());
    }
    Ok(out)
}
