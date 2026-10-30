//! The code of the module `hdf5-recons-float` (FORMAT.md, §5), which needs no aqr-formula, so
//! that aqr-modules builds it into its Wasm module.

use aqr_common::varint;

use crate::float;
use crate::{Result, corrupt};

/// The chunks of a dataset of floats, as the module gives them: `chunks` of `elements` each,
/// binary64 where `wide` (else binary32), big endian where `be`, shuffled where `shuffle`
/// (HDF5's shuffle filter), their `shift` low bits zero, predicted by `order` coefficients.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Floats {
    pub chunks: u64,
    pub elements: u64,
    pub wide: bool,
    pub be: bool,
    pub shuffle: bool,
    pub shift: u8,
    pub order: u8,
}

impl Floats {
    pub fn bytes(&self) -> usize {
        if self.wide { 8 } else { 4 }
    }

    /// Samples in all, if they fit.
    pub fn samples(&self) -> Option<usize> {
        usize::try_from(self.chunks.checked_mul(self.elements)?).ok()
    }

    /// The output's bytes, if they fit.
    pub fn out_len(&self) -> Option<u64> {
        self.chunks.checked_mul(self.elements)?.checked_mul(self.bytes() as u64)
    }

    /// The params: varints of the chunks and their elements, then a byte of flags (1 binary64,
    /// 2 big endian, 4 shuffled), the shift and the order.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut v = Vec::new();
        varint::put(&mut v, self.chunks);
        varint::put(&mut v, self.elements);
        v.push(self.wide as u8 | (self.be as u8) << 1 | (self.shuffle as u8) << 2);
        v.extend([self.shift, self.order]);
        v
    }

    pub fn from_bytes(b: &[u8]) -> Option<Floats> {
        let mut at = 0;
        let chunks = varint::get(b, &mut at).ok()?;
        let elements = varint::get(b, &mut at).ok()?;
        let &[flags, shift, order] = b.get(at..)? else { return None };
        let (wide, be, shuffle) = (flags & 1 != 0, flags & 2 != 0, flags & 4 != 0);
        let f = Floats { chunks, elements, wide, be, shuffle, shift, order };
        let valid = flags < 8 && (shift as usize) < 8 * f.bytes() && order as usize <= float::MAX_ORDER && elements > 0;
        valid.then_some(f)
    }

    /// The bits of the samples of `chunk`, the bytes of a chunk (as many as its elements
    /// take), appended to `out`.
    pub fn samples_of(&self, chunk: &[u8], out: &mut Vec<u64>) {
        let s = self.bytes();
        let n = chunk.len() / s;
        let byte = |i: usize, j: usize| if self.shuffle { chunk[j * n + i] } else { chunk[i * s + j] };
        for i in 0..n {
            let mut v = 0u64;
            for j in 0..s {
                let b = byte(i, if self.be { j } else { s - 1 - j }) as u64;
                v = v << 8 | b;
            }
            out.push(v);
        }
    }

    /// The bytes of a chunk of `samples`, appended to `out`.
    pub fn chunk_of(&self, samples: &[u64], out: &mut Vec<u8>) {
        let s = self.bytes();
        let n = samples.len();
        let at = out.len();
        out.resize(at + n * s, 0);
        let o = &mut out[at..];
        for (i, &v) in samples.iter().enumerate() {
            for j in 0..s {
                let b = (v >> (8 * if self.be { s - 1 - j } else { j })) as u8;
                o[if self.shuffle { j * n + i } else { i * s + j }] = b;
            }
        }
    }
}

/// Whether `n` bytes of output may come of `coded` bytes: not more than 64 times them and 64
/// (a sample takes a bit at least), which keeps a small input from claiming a large output.
pub fn within(n: u64, coded: usize) -> bool {
    n <= (coded as u64 + 64) * 64
}

/// `hdf5-recons-float`: the chunks of `len` bytes from `params` ([`Floats`]) and `inputs` (the
/// coefficients, the arithmetic code and the raw bits).
pub fn float(params: &[u8], inputs: &[&[u8]], len: u64) -> Result<Vec<u8>> {
    let p = Floats::from_bytes(params).ok_or_else(corrupt)?;
    let &[coefs, model, raw] = inputs else { return Err(corrupt()) };
    if p.out_len() != Some(len) || !within(len, model.len() + raw.len()) {
        return Err(corrupt());
    }
    let n = p.samples().ok_or_else(corrupt)?;
    let bits = float::decode(coefs, model, raw, n, p.wide, p.shift as u32, p.order as usize).ok_or_else(corrupt)?;
    let mut out = Vec::with_capacity(len as usize);
    for c in bits.chunks(p.elements as usize) {
        p.chunk_of(c, &mut out);
    }
    Ok(out)
}
