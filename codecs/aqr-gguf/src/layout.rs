//! The runs of an application (FORMAT.md §4): their kinds, their parameters, and how the bytes of
//! a chunk of a run are taken apart into symbols, raw bytes and side bytes, and put back.

use aqr_common::varint;

use crate::rans::{LANES, Table};
use crate::{Result, corrupt};

/// A float of the tensors: its bytes, bits of its exponent and of its mantissa.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Float {
    Bf16,
    F16,
    F32,
}

impl Float {
    pub const ALL: [Float; 3] = [Float::Bf16, Float::F16, Float::F32];

    pub const fn bytes(self) -> usize {
        match self {
            Float::Bf16 | Float::F16 => 2,
            Float::F32 => 4,
        }
    }

    pub const fn exp_bits(self) -> u32 {
        match self {
            Float::Bf16 | Float::F32 => 8,
            Float::F16 => 5,
        }
    }

    pub const fn man_bits(self) -> u32 {
        match self {
            Float::Bf16 => 7,
            Float::F16 => 10,
            Float::F32 => 23,
        }
    }

    /// Its tag in the parameters.
    pub const fn tag(self) -> u8 {
        self as u8 + 1
    }
}

/// The shape of the bits a float keeps beyond its exponent, its mantissa's `shift` low bits
/// dropped: `k` bytes as they are, and the `b` bits above them a symbol.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Kept {
    pub shift: u32,
    pub k: usize,
    pub b: u32,
}

impl Kept {
    /// Of `f`, its mantissa's `shift` low bits dropped (at most its bits); None where the bits
    /// above the bytes are more than 4.
    pub fn of(f: Float, shift: u32) -> Option<Kept> {
        if shift > f.man_bits() {
            return None;
        }
        let r = 1 + f.man_bits() - shift;
        let (k, b) = ((r / 8) as usize, r % 8);
        (b <= 4).then_some(Kept { shift, k, b })
    }

    /// The bits kept of `f`: its sign, then its mantissa without the low `shift` bits.
    #[inline(always)]
    pub fn bits(self, f: Float, v: u32) -> u32 {
        let m = f.man_bits();
        let sign = v >> (f.exp_bits() + m);
        sign << (m - self.shift) | (v & ((1 << m) - 1)) >> self.shift
    }

    /// The float of exponent `e` and the bits kept `kept` ([`Kept::bits`]).
    #[inline(always)]
    pub fn float(self, f: Float, e: u32, kept: u32) -> u32 {
        let m = f.man_bits();
        let ms = m - self.shift;
        (kept >> ms) << (f.exp_bits() + m) | e << m | (kept & ((1 << ms) - 1)) << self.shift
    }
}

/// A piece of a block of quantised weights.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Part {
    /// Bytes as they are, to the side.
    Side(usize),
    /// Bytes of two nibbles, each a symbol: of each 16 bytes, a group of their low nibbles then
    /// their high ones.
    Nib(usize),
    /// Bytes of four 2-bit fields, each a symbol: of each 8 bytes, a group of their bits 0–1,
    /// then 2–3, 4–5 and 6–7.
    Crumb(usize),
    /// Bytes whose high nibble is a symbol and low nibble raw: of each 32 bytes, a group of their
    /// high nibbles, and 16 raw bytes of the low nibbles of bytes `i` and `16 + i` (low, high).
    HiNib(usize),
    /// 32 quants of 5 bits, Q5_0's and Q5_1's: 4 bytes of their fifth bits (bit `j` of the
    /// little-endian 32 bits that of quant `j`), then 16 bytes of their low nibbles (quant `j`
    /// the low nibble of byte `j`, `16 + j` its high one). A group of the quants' high 4 bits,
    /// and 4 raw bytes of their lowest bits (bit `j` of the little-endian 32 bits, quant `j`'s).
    Five32,
    /// 256 quants of 5 bits, Q5_K's: 32 bytes `qh` of their fifth bits, then 128 bytes `qs` of
    /// their low nibbles: of quarter `k` (0 to 3), quant `64k + l` the low nibble of
    /// `qs[32k + l]` and bit `2k` of `qh[l]`, quant `64k + 32 + l` the high nibble and bit
    /// `2k + 1`. 8 groups of the quants' high 4 bits, in their order, and 32 raw bytes of their
    /// lowest bits (bit `j` of the little-endian 256 bits, quant `j`'s).
    Five256,
}

impl Part {
    pub const fn bytes(self) -> usize {
        match self {
            Part::Side(n) | Part::Nib(n) | Part::Crumb(n) | Part::HiNib(n) => n,
            Part::Five32 => 20,
            Part::Five256 => 160,
        }
    }

    /// Raw bytes of the part, of a block.
    pub const fn raw(self) -> usize {
        match self {
            Part::HiNib(n) => n / 2,
            Part::Five32 => 4,
            Part::Five256 => 32,
            _ => 0,
        }
    }

    /// Groups of symbols (of 32) of the part, of a block; 0 for a side part.
    pub const fn groups(self) -> usize {
        match self {
            Part::Side(_) => 0,
            Part::Nib(n) => n / 16,
            Part::Crumb(n) => n / 8,
            Part::HiNib(n) => n / 32,
            Part::Five32 => 1,
            Part::Five256 => 8,
        }
    }
}

/// A type of quantised blocks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Quant {
    Q4_0,
    Q4_1,
    Q8_0,
    Q4K,
    Q6K,
    Iq4Nl,
    Iq4Xs,
    Mxfp4,
    Q5_0,
    Q5_1,
    Q5K,
}

impl Quant {
    pub const ALL: [Quant; 11] = [
        Quant::Q4_0,
        Quant::Q4_1,
        Quant::Q8_0,
        Quant::Q4K,
        Quant::Q6K,
        Quant::Iq4Nl,
        Quant::Iq4Xs,
        Quant::Mxfp4,
        Quant::Q5_0,
        Quant::Q5_1,
        Quant::Q5K,
    ];

    /// Its pieces, in the order of the block's bytes.
    pub const fn parts(self) -> &'static [Part] {
        use Part::*;
        match self {
            Quant::Q4_0 | Quant::Iq4Nl => &[Side(2), Nib(16)],
            Quant::Q4_1 => &[Side(4), Nib(16)],
            Quant::Q8_0 => &[Side(2), HiNib(32)],
            Quant::Q4K => &[Side(16), Nib(128)],
            Quant::Q6K => &[Nib(128), Crumb(64), Side(18)],
            Quant::Iq4Xs => &[Side(8), Nib(128)],
            Quant::Mxfp4 => &[Side(1), Nib(16)],
            Quant::Q5_0 => &[Side(2), Five32],
            Quant::Q5_1 => &[Side(4), Five32],
            Quant::Q5K => &[Side(16), Five256],
        }
    }

    /// Bytes of a block.
    pub const fn block(self) -> usize {
        let p = self.parts();
        let mut n = 0;
        let mut i = 0;
        while i < p.len() {
            n += p[i].bytes();
            i += 1;
        }
        n
    }

    /// Weights of a block.
    pub const fn weights(self) -> usize {
        match self {
            Quant::Q4K | Quant::Q6K | Quant::Iq4Xs | Quant::Q5K => 256,
            _ => 32,
        }
    }

    /// Blocks of a chunk: 8192 weights.
    pub const fn chunk(self) -> usize {
        CHUNK_WEIGHTS / self.weights()
    }

    /// Parts of symbols, each with a table.
    pub fn coded(self) -> impl Iterator<Item = Part> {
        self.parts().iter().copied().filter(|p| p.groups() > 0)
    }

    pub const fn tag(self) -> u8 {
        0x10 + self as u8
    }

    /// The type of ggml's number `t`.
    pub fn of_ggml(t: u32) -> Option<Quant> {
        Some(match t {
            2 => Quant::Q4_0,
            3 => Quant::Q4_1,
            8 => Quant::Q8_0,
            12 => Quant::Q4K,
            14 => Quant::Q6K,
            20 => Quant::Iq4Nl,
            23 => Quant::Iq4Xs,
            39 => Quant::Mxfp4,
            6 => Quant::Q5_0,
            7 => Quant::Q5_1,
            13 => Quant::Q5K,
            _ => return None,
        })
    }
}

/// Weights of a chunk of quantised blocks.
pub const CHUNK_WEIGHTS: usize = 8192;
/// Floats of a chunk.
pub const FLOAT_CHUNK: usize = 2048;

/// Bytes of symbols a chunk decodes to at most: a chunk of Q6_K's (32 × (8 + 8) groups).
pub const MAX_CHUNK_SYMS: usize = 16 << 10;
/// Side bytes of a chunk at most: a chunk of floats' escapes (whole floats), or of blocks' side
/// parts.
pub const MAX_CHUNK_SIDE: usize = 4 * FLOAT_CHUNK;
/// Raw bytes of a chunk at most: 3 for each float of a chunk.
pub const MAX_CHUNK_RAW: usize = 3 * FLOAT_CHUNK;

/// The tables of the symbols of a float: of the exponent and of the bits above the bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FloatRun {
    pub f: Float,
    pub kept: Kept,
    pub n: u64,
    /// The exponents of the symbols below `m`; the symbols from `m` on are escapes.
    pub map: [u8; 16],
    pub m: u8,
    /// Whether an escape is a whole float in the side bytes (else its exponent, a byte).
    pub whole: bool,
    pub exp: Table,
    pub top: Option<Table>,
}

/// A run of an application.
// (a run of floats inline: the runs are read one at a time)
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Run {
    /// Bytes as they are, to the side.
    Raw(u64),
    Float(FloatRun),
    /// Blocks of a type, a table for each part of symbols.
    Quant {
        q: Quant,
        blocks: u64,
        tables: Vec<Table>,
    },
}

fn num(p: &[u8], at: &mut usize) -> Result<u64> {
    varint::get(p, at).map_err(|_| corrupt())
}

fn byte(p: &[u8], at: &mut usize) -> Result<u8> {
    let b = *p.get(*at).ok_or_else(corrupt)?;
    *at += 1;
    Ok(b)
}

fn table(p: &[u8], at: &mut usize) -> Result<Table> {
    let mut f = [0u32; 16];
    for v in f.iter_mut() {
        *v = u32::try_from(num(p, at)?).map_err(|_| corrupt())?;
    }
    Table::new(f)
}

fn put_table(out: &mut Vec<u8>, t: &Table) {
    for &f in &t.freq {
        varint::put(out, f as u64);
    }
}

impl Run {
    /// Bytes of its output.
    pub fn len(&self) -> u64 {
        match self {
            Run::Raw(n) => *n,
            Run::Float(r) => r.n * r.f.bytes() as u64,
            Run::Quant { q, blocks, .. } => blocks * q.block() as u64,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The next run of parameters `p` from `*at`.
    pub fn read(p: &[u8], at: &mut usize) -> Result<Run> {
        let tag = byte(p, at)?;
        if tag == 0 {
            return Ok(Run::Raw(num(p, at)?));
        }
        if let Some(&f) = Float::ALL.iter().find(|f| f.tag() == tag) {
            let shift = byte(p, at)? as u32;
            let kept = Kept::of(f, shift).ok_or_else(corrupt)?;
            let n = num(p, at)?;
            let mb = byte(p, at)?;
            let (m, whole) = (mb & 0x7f, mb & 0x80 != 0);
            if !(1..=16).contains(&m) {
                return Err(corrupt());
            }
            let mut map = [0u8; 16];
            for v in map.iter_mut().take(m as usize) {
                *v = byte(p, at)?;
                if *v as u32 >= 1 << f.exp_bits() {
                    return Err(corrupt());
                }
            }
            let exp = table(p, at)?;
            let top = if kept.b > 0 { Some(table(p, at)?) } else { None };
            if let Some(t) = &top
                && t.freq[1 << kept.b..].iter().any(|&f| f > 0)
            {
                return Err(corrupt());
            }
            return Ok(Run::Float(FloatRun { f, kept, n, map, m, whole, exp, top }));
        }
        if let Some(&q) = Quant::ALL.iter().find(|q| q.tag() == tag) {
            let blocks = num(p, at)?;
            let tables = q.coded().map(|_| table(p, at)).collect::<Result<Vec<_>>>()?;
            for (part, t) in q.coded().zip(&tables) {
                if matches!(part, Part::Crumb(_)) && t.freq[4..].iter().any(|&f| f > 0) {
                    return Err(corrupt());
                }
            }
            return Ok(Run::Quant { q, blocks, tables });
        }
        Err(corrupt())
    }

    /// Appends its parameters to `out`.
    pub fn write(&self, out: &mut Vec<u8>) {
        match self {
            Run::Raw(n) => {
                out.push(0);
                varint::put(out, *n);
            }
            Run::Float(r) => {
                out.push(r.f.tag());
                out.push(r.kept.shift as u8);
                varint::put(out, r.n);
                out.push(r.m | (r.whole as u8) << 7);
                out.extend_from_slice(&r.map[..r.m as usize]);
                put_table(out, &r.exp);
                if let Some(t) = &r.top {
                    put_table(out, t);
                }
            }
            Run::Quant { q, blocks, tables } => {
                out.push(q.tag());
                varint::put(out, *blocks);
                for t in tables {
                    put_table(out, t);
                }
            }
        }
    }
}

/// The groups of a chunk of `n` floats: `ceil(n / 32)`, the last of `n % 32` where not 0.
pub fn float_groups(n: usize) -> impl Iterator<Item = usize> {
    (0..n.div_ceil(LANES)).map(move |g| (n - g * LANES).min(LANES))
}

/// A chunk of blocks of `q`, `d`, taken apart (FORMAT.md §4.3): into `syms` the symbols of each
/// part of symbols in turn, into `raw` the raw bytes and into `side` the side bytes, each of
/// the length the chunk has of them.
pub fn split_quant(q: Quant, d: &[u8], syms: &mut [u8], raw: &mut [u8], side: &mut [u8]) {
    let bs = q.block();
    let blocks = d.len() / bs;
    debug_assert_eq!(blocks * bs, d.len());
    let (mut at, mut s_at, mut side_at, mut raw_at) = (0, 0, 0, 0);
    for part in q.parts() {
        let n = part.bytes();
        match *part {
            Part::Side(_) => {
                for b in 0..blocks {
                    side[side_at..side_at + n].copy_from_slice(&d[b * bs + at..][..n]);
                    side_at += n;
                }
            }
            Part::Nib(_) => {
                let s = &mut syms[s_at..s_at + blocks * n * 2];
                for b in 0..blocks {
                    let src = &d[b * bs + at..][..n];
                    for (c, g) in src.as_chunks::<16>().0.iter().zip(s[b * n * 2..][..n * 2].as_chunks_mut::<32>().0) {
                        for i in 0..16 {
                            g[i] = c[i] & 15;
                            g[16 + i] = c[i] >> 4;
                        }
                    }
                }
                s_at += blocks * n * 2;
            }
            Part::Crumb(_) => {
                let s = &mut syms[s_at..s_at + blocks * n * 4];
                for b in 0..blocks {
                    let src = &d[b * bs + at..][..n];
                    for (c, g) in src.as_chunks::<8>().0.iter().zip(s[b * n * 4..][..n * 4].as_chunks_mut::<32>().0) {
                        for i in 0..8 {
                            g[i] = c[i] & 3;
                            g[8 + i] = c[i] >> 2 & 3;
                            g[16 + i] = c[i] >> 4 & 3;
                            g[24 + i] = c[i] >> 6;
                        }
                    }
                }
                s_at += blocks * n * 4;
            }
            Part::HiNib(_) => {
                let s = &mut syms[s_at..s_at + blocks * n];
                for b in 0..blocks {
                    let src = &d[b * bs + at..][..n];
                    for (c, g) in src.as_chunks::<32>().0.iter().zip(s[b * n..][..n].as_chunks_mut::<32>().0) {
                        let r = &mut raw[raw_at..][..16];
                        raw_at += 16;
                        for i in 0..32 {
                            g[i] = c[i] >> 4;
                        }
                        for i in 0..16 {
                            r[i] = c[i] & 15 | (c[16 + i] & 15) << 4;
                        }
                    }
                }
                s_at += blocks * n;
            }
            Part::Five32 | Part::Five256 => {
                let (quants, qh) = if *part == Part::Five32 { (32, 4) } else { (256, 32) };
                let s = &mut syms[s_at..s_at + blocks * quants];
                for b in 0..blocks {
                    let src = &d[b * bs + at..][..n];
                    let g = &mut s[b * quants..][..quants];
                    let r = &mut raw[raw_at..][..quants / 8];
                    r.fill(0);
                    for j in 0..quants {
                        let v = five(src, qh, j);
                        g[j] = v >> 1;
                        r[j / 8] |= (v & 1) << (j % 8);
                    }
                    raw_at += quants / 8;
                }
                s_at += blocks * quants;
            }
        }
        at += n;
    }
}

/// Quant `j` of a part of 5-bit quants (`Part::Five32` where `qh` is 4 bytes, else
/// `Part::Five256`).
#[inline(always)]
fn five(src: &[u8], qh: usize, j: usize) -> u8 {
    let (h, qs) = src.split_at(qh);
    if qh == 4 {
        let nib = if j < 16 { qs[j] & 15 } else { qs[j - 16] >> 4 };
        nib | (h[j / 8] >> (j % 8) & 1) << 4
    } else {
        let (k, l, hi) = (j / 64, j % 32, j % 64 >= 32);
        let nib = if hi { qs[32 * k + l] >> 4 } else { qs[32 * k + l] & 15 };
        nib | (h[l] >> (2 * k + hi as usize) & 1) << 4
    }
}

/// Symbols of a chunk of `blocks` blocks of `q`.
pub fn quant_syms(q: Quant, blocks: usize) -> usize {
    q.coded().map(|p| p.groups() * LANES).sum::<usize>() * blocks
}

/// [`split_quant`] undone: the chunk of `blocks` blocks into `out` from the symbols of each part
/// (`syms`, one after another), the raw bytes and the side bytes.
pub fn join_quant(q: Quant, blocks: usize, syms: &[u8], raw: &[u8], side: &[u8], out: &mut [u8]) {
    let bs = q.block();
    debug_assert_eq!(out.len(), blocks * bs);
    let (mut at, mut s_at, mut side_at, mut raw_at) = (0, 0, 0, 0);
    for part in q.parts() {
        let n = part.bytes();
        match *part {
            Part::Side(_) => {
                for b in 0..blocks {
                    out[b * bs + at..][..n].copy_from_slice(&side[side_at..][..n]);
                    side_at += n;
                }
            }
            Part::Nib(_) => {
                let s = &syms[s_at..s_at + blocks * n * 2];
                for b in 0..blocks {
                    let o = &mut out[b * bs + at..][..n];
                    for (c, g) in o.as_chunks_mut::<16>().0.iter_mut().zip(s[b * n * 2..][..n * 2].as_chunks::<32>().0)
                    {
                        for i in 0..16 {
                            c[i] = g[i] | g[16 + i] << 4;
                        }
                    }
                }
                s_at += blocks * n * 2;
            }
            Part::Crumb(_) => {
                let s = &syms[s_at..s_at + blocks * n * 4];
                for b in 0..blocks {
                    let o = &mut out[b * bs + at..][..n];
                    for (c, g) in o.as_chunks_mut::<8>().0.iter_mut().zip(s[b * n * 4..][..n * 4].as_chunks::<32>().0) {
                        for i in 0..8 {
                            c[i] = g[i] | g[8 + i] << 2 | g[16 + i] << 4 | g[24 + i] << 6;
                        }
                    }
                }
                s_at += blocks * n * 4;
            }
            Part::HiNib(_) => {
                let s = &syms[s_at..s_at + blocks * n];
                for b in 0..blocks {
                    let o = &mut out[b * bs + at..][..n];
                    for (c, g) in o.as_chunks_mut::<32>().0.iter_mut().zip(s[b * n..][..n].as_chunks::<32>().0) {
                        let r = &raw[raw_at..][..16];
                        raw_at += 16;
                        for i in 0..16 {
                            c[i] = g[i] << 4 | r[i] & 15;
                            c[16 + i] = g[16 + i] << 4 | r[i] >> 4;
                        }
                    }
                }
                s_at += blocks * n;
            }
            Part::Five32 => {
                let s = &syms[s_at..s_at + blocks * 32];
                for b in 0..blocks {
                    let o = &mut out[b * bs + at..][..n];
                    let (g, r) = (&s[b * 32..][..32], &raw[raw_at..][..4]);
                    raw_at += 4;
                    let v = |j: usize| g[j] << 1 | r[j / 8] >> (j % 8) & 1;
                    let (h, qs) = o.split_at_mut(4);
                    h.fill(0);
                    for j in 0..16 {
                        let (a, c) = (v(j), v(16 + j));
                        qs[j] = a & 15 | (c & 15) << 4;
                        h[j / 8] |= (a >> 4) << (j % 8);
                        h[2 + j / 8] |= (c >> 4) << (j % 8);
                    }
                }
                s_at += blocks * 32;
            }
            Part::Five256 => {
                let s = &syms[s_at..s_at + blocks * 256];
                for b in 0..blocks {
                    let o = &mut out[b * bs + at..][..n];
                    let (g, r) = (&s[b * 256..][..256], &raw[raw_at..][..32]);
                    raw_at += 32;
                    let v = |j: usize| g[j] << 1 | r[j / 8] >> (j % 8) & 1;
                    let (h, qs) = o.split_at_mut(32);
                    h.fill(0);
                    for k in 0..4 {
                        for l in 0..32 {
                            let (a, c) = (v(64 * k + l), v(64 * k + 32 + l));
                            qs[32 * k + l] = a & 15 | (c & 15) << 4;
                            h[l] |= (a >> 4) << (2 * k) | (c >> 4) << (2 * k + 1);
                        }
                    }
                }
                s_at += blocks * 256;
            }
        }
        at += n;
    }
}

/// Raw bytes of a chunk of `blocks` blocks of `q`.
pub fn quant_raw(q: Quant, blocks: usize) -> usize {
    q.parts().iter().map(|p| p.raw()).sum::<usize>() * blocks
}

/// Side bytes of a chunk of `blocks` blocks of `q`.
pub fn quant_side(q: Quant, blocks: usize) -> usize {
    q.parts().iter().map(|p| if let Part::Side(n) = p { *n } else { 0 }).sum::<usize>() * blocks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks() {
        let sizes: Vec<usize> = Quant::ALL.iter().map(|q| q.block()).collect();
        assert_eq!(sizes, [18, 20, 34, 144, 210, 18, 136, 17, 22, 24, 176]);
        assert!(
            Quant::ALL.iter().all(|q| q.coded().map(|p| p.groups() * 32).sum::<usize>() * q.chunk() <= MAX_CHUNK_SYMS)
        );
        assert!(Quant::ALL.iter().all(|&q| quant_side(q, q.chunk()) <= MAX_CHUNK_SIDE));
        assert!(Quant::ALL.iter().all(|&q| quant_raw(q, q.chunk()) <= MAX_CHUNK_RAW));
    }

    #[test]
    fn quant_roundtrip() {
        for q in Quant::ALL {
            let d: Vec<u8> = (0..q.block() * 7).map(|i| (i * 131 + i / 7) as u8).collect();
            let mut all = vec![0; quant_syms(q, 7)];
            let (mut raw, mut side) = (vec![0; quant_raw(q, 7)], vec![0; quant_side(q, 7)]);
            split_quant(q, &d, &mut all, &mut raw, &mut side);
            let mut out = vec![0; d.len()];
            join_quant(q, 7, &all, &raw, &side, &mut out);
            assert_eq!(out, d, "{q:?}");
        }
    }

    #[test]
    fn kept_bits() {
        for f in Float::ALL {
            for shift in 0..=f.man_bits() {
                let Some(k) = Kept::of(f, shift) else { continue };
                let w = 8 * f.bytes() as u32;
                for v in [0u32, 1, 0x1234_5678, 0xFFFF_FFFF, 0x8000_0001, 0x7F80_0000] {
                    let v = if w == 32 { v } else { v & 0xFFFF };
                    let v = v & !((1 << shift) - 1);
                    let e = v >> f.man_bits() & ((1 << f.exp_bits()) - 1);
                    let bits = k.bits(f, v);
                    assert!(bits < 1 << (8 * k.k as u32 + k.b));
                    assert_eq!(k.float(f, e, bits), v, "{f:?} {shift} {v:x}");
                }
            }
        }
        // (every shift has one at most a few bits lower that fits)
        assert!(Float::ALL.iter().all(|&f| Kept::of(f, 0).is_some()));
    }
}
