//! The runs of an application (FORMAT.md §4): their kinds, their parameters, and how the bytes of
//! a chunk of a run are taken apart into symbols, raw bytes and side bytes, and put back.

use aqr_common::varint;

use crate::rans::{LANES, Table};
use crate::{Result, corrupt};

/// A float of the tensors: its bytes, bits of its exponent and of its mantissa (and a sign).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Float {
    Bf16,
    F16,
    F32,
    F64,
    /// FP8 E4M3 (`float8_e4m3fn`: no infinities, its NaN all ones).
    E4m3,
    /// FP8 E5M2.
    E5m2,
}

impl Float {
    pub const ALL: [Float; 6] = [Float::Bf16, Float::F16, Float::F32, Float::F64, Float::E4m3, Float::E5m2];

    pub const fn bytes(self) -> usize {
        match self {
            Float::Bf16 | Float::F16 => 2,
            Float::F32 => 4,
            Float::F64 => 8,
            Float::E4m3 | Float::E5m2 => 1,
        }
    }

    pub const fn exp_bits(self) -> u32 {
        match self {
            Float::Bf16 | Float::F32 => 8,
            Float::F16 | Float::E5m2 => 5,
            Float::F64 => 11,
            Float::E4m3 => 4,
        }
    }

    pub const fn man_bits(self) -> u32 {
        match self {
            Float::Bf16 => 7,
            Float::F16 => 10,
            Float::F32 => 23,
            Float::F64 => 52,
            Float::E4m3 => 3,
            Float::E5m2 => 2,
        }
    }

    /// Bytes of an escape's exponent in the side bytes.
    pub const fn exp_bytes(self) -> usize {
        (self.exp_bits() as usize).div_ceil(8)
    }

    /// Its tag in the parameters (of a run of floats; of a grid's output, FORMAT.md §4.5).
    pub const fn tag(self) -> u8 {
        self as u8 + 1
    }

    /// The float of tag `t`.
    pub fn of_tag(t: u8) -> Option<Float> {
        Float::ALL.iter().copied().find(|f| f.tag() == t)
    }

    /// The exponent of `v`.
    #[inline(always)]
    pub fn exp(self, v: u64) -> u32 {
        (v >> self.man_bits()) as u32 & ((1 << self.exp_bits()) - 1)
    }
}

/// The bits a float keeps beyond its exponent: its sign, then its mantissa without the low
/// `shift` bits (which are zero), `1 + M − shift` bits in all.
#[inline(always)]
pub fn kept_bits(f: Float, shift: u32, v: u64) -> u64 {
    let m = f.man_bits();
    let sign = v >> (f.exp_bits() + m) & 1;
    sign << (m - shift) | (v & ((1 << m) - 1)) >> shift
}

/// The float of exponent `e` and the bits kept `kept` ([`kept_bits`]).
#[inline(always)]
pub fn kept_float(f: Float, shift: u32, e: u64, kept: u64) -> u64 {
    let m = f.man_bits();
    let ms = m - shift;
    (kept >> ms) << (f.exp_bits() + m) | e << m | (kept & ((1 << ms) - 1)) << shift
}

/// `bits` bits of `v` put at bit `at` of `out` (little-endian bits; `out` zero there, with 8
/// bytes past them).
#[inline(always)]
pub fn put_bits(out: &mut [u8], at: usize, bits: u32, v: u64) {
    let (i, o) = (at / 8, at % 8);
    let mut w = u64::from_le_bytes(out[i..i + 8].try_into().unwrap());
    w |= v << o;
    out[i..i + 8].copy_from_slice(&w.to_le_bytes());
    if o as u32 + bits > 64 {
        out[i + 8] |= (v >> (64 - o)) as u8;
    }
}

/// `bits` bits (at most 56) at bit `at` of `d` (with 8 bytes past them).
#[inline(always)]
pub fn get_bits(d: &[u8], at: usize, bits: u32) -> u64 {
    let w = u64::from_le_bytes(d[at / 8..at / 8 + 8].try_into().unwrap());
    (w >> (at % 8)) & ((1u64 << bits) - 1)
}

/// The float of type `f` (BF16, F16 or F32) nearest `x`, ties to even (F16's subnormals made,
/// beyond its range infinity); a NaN is the quiet NaN of `f`, positive. The value a grid's
/// element is (FORMAT.md §4.5).
#[inline(always)]
pub fn round_f32(f: Float, x: f32) -> u64 {
    let u = x.to_bits();
    if x.is_nan() {
        return match f {
            Float::Bf16 => 0x7fc0,
            Float::F16 => 0x7e00,
            _ => 0x7fc0_0000,
        };
    }
    match f {
        Float::Bf16 => ((u + 0x7fff + (u >> 16 & 1)) >> 16) as u64,
        Float::F16 => f32_to_f16(u) as u64,
        _ => u as u64,
    }
}

/// F16 of the bits of an F32 (not a NaN), round to nearest even.
#[inline(always)]
fn f32_to_f16(u: u32) -> u16 {
    let sign = (u >> 16 & 0x8000) as u16;
    let a = u & 0x7fff_ffff;
    // (2^16 and above, or infinity: infinity; 65520 rounds up to it)
    if a >= 0x477f_f000 {
        return sign | 0x7c00;
    }
    // (below 2^-14: a subnormal, the mantissa with its leading one shifted, to nearest even)
    if a < 0x3880_0000 {
        let e = (a >> 23) as i32;
        // (below half the least subnormal: zero)
        if e < 102 {
            return sign;
        }
        let m = a & 0x7f_ffff | 0x80_0000;
        let s = (126 - e) as u32;
        let (q, r, half) = (m >> s, m & ((1 << s) - 1), 1 << (s - 1));
        let q = q + (r > half || r == half && q & 1 == 1) as u32;
        return sign | q as u16;
    }
    let v = a - (112 << 23);
    let (q, r) = (v >> 13, v & 0x1fff);
    let q = q + (r > 0x1000 || r == 0x1000 && q & 1 == 1) as u32;
    sign | q as u16
}

/// A piece of a block of quantised weights.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Part {
    /// Bytes as they are, to the side.
    Side(usize),
    /// Bytes as they are, raw.
    Raw(usize),
    /// Bytes of two nibbles, each a symbol: of each 16 bytes (or of all where fewer), their low
    /// nibbles then their high ones.
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
    /// 256 quants of 3 bits, Q3_K's: 32 bytes `hmask` of their high bits, then 64 bytes `qs` of
    /// their low 2 bits: of half `h` (0 or 1), `m` (0 to 3) and `l` (0 to 31), quant
    /// `128h + 32m + l` is bits `2m` and `2m + 1` of `qs[32h + l]` and bit `4h + m` of
    /// `hmask[l]` (its high bit). Its symbols the quants, in their order (each below 8).
    Three256,
    /// IQ1_S's `qh`, 8 little-endian 16-bit numbers: their bits 12–14 the symbols (each below
    /// 8), and 13 raw bytes: their low 12 bits (number `i`'s bits `12i` to `12i + 11` of the
    /// little-endian 96 bits), then a byte of their top bits (bit `i` number `i`'s).
    Iq1sHigh,
    /// IQ1_M's scales, 4 little-endian 16-bit numbers: their 3-bit fields `k` (0 to 3, bits `3k`
    /// to `3k + 2`) the symbols (number `i`'s field `k` symbol `4i + k`, each below 8), and 2 raw
    /// bytes of their top nibbles (number `2j`'s the low nibble of byte `j`, `2j + 1`'s its high
    /// nibble: the block's F16 scale).
    Iq1mScales,
    /// Little-endian 32-bit numbers (`n` bytes, a multiple of 8): their top nibbles the symbols,
    /// and of each two, 7 raw bytes of their low 28 bits (the first's the low ones of the
    /// little-endian 56 bits).
    TopNib(usize),
    /// IQ2_XXS's `qs`, 8 pairs of little-endian 32-bit numbers: the top nibbles of the second of
    /// each pair the symbols, and 60 raw bytes: the first of each pair, as they are, then the
    /// second's as [`Part::TopNib`]'s.
    Iq2Xxs,
}

impl Part {
    pub const fn bytes(self) -> usize {
        match self {
            Part::Side(n) | Part::Raw(n) | Part::Nib(n) | Part::Crumb(n) | Part::HiNib(n) | Part::TopNib(n) => n,
            Part::Five32 => 20,
            Part::Five256 => 160,
            Part::Three256 => 96,
            Part::Iq1sHigh => 16,
            Part::Iq1mScales => 8,
            Part::Iq2Xxs => 64,
        }
    }

    /// Raw bytes of the part, of a block.
    pub const fn raw(self) -> usize {
        match self {
            Part::Raw(n) => n,
            Part::HiNib(n) => n / 2,
            Part::Five32 => 4,
            Part::Five256 => 32,
            Part::Iq1sHigh => 13,
            Part::Iq1mScales => 2,
            Part::TopNib(n) => n / 8 * 7,
            Part::Iq2Xxs => 60,
            _ => 0,
        }
    }

    /// Symbols of the part, of a block; 0 for a part of no symbols.
    pub const fn syms(self) -> usize {
        match self {
            Part::Side(_) | Part::Raw(_) => 0,
            Part::Nib(n) => 2 * n,
            Part::Crumb(n) => 4 * n,
            Part::HiNib(n) => n,
            Part::Five32 => 32,
            Part::Five256 | Part::Three256 => 256,
            Part::Iq1sHigh | Part::Iq2Xxs => 8,
            Part::Iq1mScales => 16,
            Part::TopNib(n) => n / 4,
        }
    }

    /// The symbols of the part are below this.
    pub const fn alphabet(self) -> usize {
        match self {
            Part::Crumb(_) => 4,
            Part::Three256 | Part::Iq1sHigh | Part::Iq1mScales => 8,
            _ => 16,
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
    Q2K,
    Q3K,
    Iq2Xxs,
    Iq2Xs,
    Iq3Xxs,
    Iq1S,
    Iq3S,
    Iq2S,
    Iq1M,
}

impl Quant {
    pub const ALL: [Quant; 20] = [
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
        Quant::Q2K,
        Quant::Q3K,
        Quant::Iq2Xxs,
        Quant::Iq2Xs,
        Quant::Iq3Xxs,
        Quant::Iq1S,
        Quant::Iq3S,
        Quant::Iq2S,
        Quant::Iq1M,
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
            // (the quants of the types below 4 bits, but Q2_K's and Q3_K's, are of codebooks: their
            // indices and signs raw, near their entropy; their scales the symbols)
            Quant::Q2K => &[Nib(16), Crumb(64), Side(4)],
            Quant::Q3K => &[Three256, Side(14)],
            Quant::Iq2Xxs => &[Side(2), Iq2Xxs],
            Quant::Iq2Xs => &[Side(2), Raw(64), Nib(8)],
            Quant::Iq3Xxs => &[Side(2), Raw(64), TopNib(32)],
            Quant::Iq1S => &[Side(2), Raw(32), Iq1sHigh],
            Quant::Iq3S => &[Side(2), Raw(104), Nib(4)],
            Quant::Iq2S => &[Side(2), Raw(72), Nib(8)],
            Quant::Iq1M => &[Raw(48), Iq1mScales],
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
            Quant::Q4_0 | Quant::Q4_1 | Quant::Q8_0 | Quant::Iq4Nl | Quant::Mxfp4 | Quant::Q5_0 | Quant::Q5_1 => 32,
            _ => 256,
        }
    }

    /// Blocks of a chunk: 8192 weights.
    pub const fn chunk(self) -> usize {
        CHUNK_WEIGHTS / self.weights()
    }

    /// Parts of symbols, each with a table.
    pub fn coded(self) -> impl Iterator<Item = Part> {
        self.parts().iter().copied().filter(|p| p.syms() > 0)
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
            10 => Quant::Q2K,
            11 => Quant::Q3K,
            16 => Quant::Iq2Xxs,
            17 => Quant::Iq2Xs,
            18 => Quant::Iq3Xxs,
            19 => Quant::Iq1S,
            21 => Quant::Iq3S,
            22 => Quant::Iq2S,
            29 => Quant::Iq1M,
            _ => return None,
        })
    }
}

/// Weights of a chunk of quantised blocks.
pub const CHUNK_WEIGHTS: usize = 8192;
/// Floats of a chunk (of a run of floats, or of a grid).
pub const FLOAT_CHUNK: usize = 2048;
/// Bytes of the elements of a chunk of a run of lanes.
pub const LANES_CHUNK: usize = 8192;

/// Bytes of symbols a chunk decodes to at most: a chunk of Q6_K's (32 × (8 + 8) groups), or of
/// lanes all of nibbles.
pub const MAX_CHUNK_SYMS: usize = 16 << 10;
/// Side bytes of a chunk at most: a chunk of floats' escapes (whole floats of F64), or of blocks'
/// side parts. (A grid reads its side bytes as it goes.)
pub const MAX_CHUNK_SIDE: usize = 8 * FLOAT_CHUNK;
/// Raw bytes of a chunk at most, with 8 past them for the bits read 8 bytes at a time: a chunk of
/// F64's 53 bits each, or of lanes all raw.
pub const MAX_CHUNK_RAW: usize = 16 << 10;
/// Columns of tiles of a grid at most (FORMAT.md §4.5).
pub const MAX_TILES: u64 = 1 << 14;
/// Values of a grid at most.
pub const MAX_GRID: usize = 256;

/// A run of floats (FORMAT.md §4.2): the exponents' symbols by `map`, then the top `t` bits of the
/// bits kept of each (its sign and mantissa, without the `shift` low bits of its mantissa) a
/// symbol of the table of its exponent symbol's class, the rest raw.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FloatRun {
    pub f: Float,
    pub shift: u32,
    pub n: u64,
    /// The exponents of the symbols below `m`; the symbols from `m` on are escapes.
    pub map: [u16; 16],
    pub m: u8,
    /// Whether an escape is a whole float in the side bytes (else its exponent).
    pub whole: bool,
    pub exp: Table,
    /// Bits of the top of the bits kept that are a symbol (0 to 4).
    pub t: u32,
    /// The class of each exponent symbol: of its table in `man`.
    pub class: [u8; 16],
    /// The tables of the top bits, one for each class (none where `t` is 0).
    pub man: Vec<Table>,
}

impl FloatRun {
    /// Bits kept of each float.
    pub fn kept(&self) -> u32 {
        1 + self.f.man_bits() - self.shift
    }

    /// Raw bits of each float.
    pub fn raw_bits(&self) -> u32 {
        self.kept() - self.t
    }
}

/// A run of elements of `w` bytes (FORMAT.md §4.4), each of their bytes (a lane) as it is, or its
/// two nibbles symbols.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LanesRun {
    pub w: usize,
    pub n: u64,
    /// The lanes of nibbles: bit `j` lane `j`.
    pub nib: u8,
    /// Of each lane of nibbles in turn, the tables of its low and of its high nibbles.
    pub tables: Vec<Table>,
}

impl LanesRun {
    /// Elements of a chunk.
    pub fn chunk(&self) -> usize {
        LANES_CHUNK / self.w
    }
}

/// The values of a grid: its own, or the grid of the run of a grid before it.
#[derive(Clone, Debug, PartialEq)]
pub enum GridValues {
    Own(Vec<f32>),
    Same,
}

/// A run of a grid (FORMAT.md §4.5): elements of type `f` (BF16, F16, F32), in rows of `row`,
/// each the value of its index in the grid times the scale of its tile (`br` rows by `bc`
/// columns), less the tile's zero point first where `zero`.
#[derive(Clone, Debug, PartialEq)]
pub struct GridRun {
    pub f: Float,
    pub n: u64,
    /// The element of the tensor the run starts at: its place in the rows and tiles.
    pub start: u64,
    pub row: u64,
    pub br: u64,
    pub bc: u64,
    pub zero: bool,
    pub grid: GridValues,
    /// The values of the grid (of the grid itself, or the one before).
    pub k: usize,
    /// Where `k` is at most 16, the table of the indices; else of their high nibbles, then of
    /// their low nibbles, one for each class.
    pub tables: Vec<Table>,
    /// Of the high nibbles, the class of each (of its table of the low nibbles).
    pub class: [u8; 16],
}

impl GridRun {
    /// Symbols of each element: 1 or 2.
    pub fn syms(&self) -> usize {
        if self.k <= 16 { 1 } else { 2 }
    }

    /// Tile columns of a row.
    pub fn tiles(&self) -> u64 {
        self.row.div_ceil(self.bc)
    }
}

/// The value of a grid's element: of grid value `g`, scale `s` and zero point `z` (where
/// `zero`), of type `f` (FORMAT.md §4.5).
#[inline(always)]
pub fn grid_value(f: Float, g: f32, s: f32, z: f32, zero: bool) -> u64 {
    round_f32(f, if zero { (g - z) * s } else { g * s })
}

/// A scale of these bits is that of a tile escaped: its elements are in the side bytes as they
/// are.
pub const ESCAPED: u32 = 0xffff_ffff;

/// A run of an application.
// (a run of floats inline: the runs are read one at a time)
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq)]
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
    Lanes(LanesRun),
    Grid(GridRun),
}

/// The tag of a run of lanes, and of a grid.
pub const LANES_TAG: u8 = 8;
pub const GRID_TAG: u8 = 9;

fn num(p: &[u8], at: &mut usize) -> Result<u64> {
    varint::get(p, at).map_err(|_| corrupt())
}

fn byte(p: &[u8], at: &mut usize) -> Result<u8> {
    let b = *p.get(*at).ok_or_else(corrupt)?;
    *at += 1;
    Ok(b)
}

/// A table of symbols below `alphabet`.
fn table(p: &[u8], at: &mut usize, alphabet: usize) -> Result<Table> {
    let mut f = [0u32; 16];
    for v in f.iter_mut() {
        *v = u32::try_from(num(p, at)?).map_err(|_| corrupt())?;
    }
    if f[alphabet..].iter().any(|&f| f > 0) {
        return Err(corrupt());
    }
    Table::new(f)
}

fn put_table(out: &mut Vec<u8>, t: &Table) {
    for &f in &t.freq {
        varint::put(out, f as u64);
    }
}

/// Classes of 16 symbols: their count (1 to 4), then where more than 1, 8 bytes of a nibble each
/// (symbol `2i`'s the low nibble of byte `i`).
fn classes(p: &[u8], at: &mut usize) -> Result<(usize, [u8; 16])> {
    let c = byte(p, at)? as usize;
    if !(1..=4).contains(&c) {
        return Err(corrupt());
    }
    let mut class = [0u8; 16];
    if c > 1 {
        for i in 0..8 {
            let b = byte(p, at)?;
            (class[2 * i], class[2 * i + 1]) = (b & 15, b >> 4);
        }
        if class.iter().any(|&k| k as usize >= c) {
            return Err(corrupt());
        }
    }
    Ok((c, class))
}

fn put_classes(out: &mut Vec<u8>, c: usize, class: &[u8; 16]) {
    out.push(c as u8);
    if c > 1 {
        out.extend((0..8).map(|i| class[2 * i] | class[2 * i + 1] << 4));
    }
}

impl Run {
    /// Bytes of its output.
    pub fn len(&self) -> u64 {
        match self {
            Run::Raw(n) => *n,
            Run::Float(r) => r.n * r.f.bytes() as u64,
            Run::Quant { q, blocks, .. } => blocks * q.block() as u64,
            Run::Lanes(r) => r.n * r.w as u64,
            Run::Grid(r) => r.n * r.f.bytes() as u64,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The tag of the run.
    pub fn tag(&self) -> u8 {
        match self {
            Run::Raw(_) => 0,
            Run::Float(r) => r.f.tag(),
            Run::Quant { q, .. } => q.tag(),
            Run::Lanes(_) => LANES_TAG,
            Run::Grid(_) => GRID_TAG,
        }
    }

    /// The next run of parameters `p` from `*at`; `k` the values of the last grid before it (0
    /// for none), a grid's of [`GridValues::Same`].
    pub fn read(p: &[u8], at: &mut usize, k: usize) -> Result<Run> {
        let tag = byte(p, at)?;
        if tag == 0 {
            return Ok(Run::Raw(num(p, at)?));
        }
        if let Some(f) = Float::of_tag(tag) {
            let shift = byte(p, at)? as u32;
            if shift > f.man_bits() {
                return Err(corrupt());
            }
            let n = num(p, at)?;
            let mb = byte(p, at)?;
            let (m, whole) = (mb & 0x7f, mb & 0x80 != 0);
            if !(1..=16).contains(&m) {
                return Err(corrupt());
            }
            let mut map = [0u16; 16];
            for v in map.iter_mut().take(m as usize) {
                let e = num(p, at)?;
                if e >= 1 << f.exp_bits() {
                    return Err(corrupt());
                }
                *v = e as u16;
            }
            let exp = table(p, at, 16)?;
            let t = byte(p, at)? as u32;
            if t > 4 || t > 1 + f.man_bits() - shift {
                return Err(corrupt());
            }
            let (mut class, mut man) = ([0u8; 16], Vec::new());
            if t > 0 {
                let c;
                (c, class) = classes(p, at)?;
                man = (0..c).map(|_| table(p, at, 1 << t)).collect::<Result<_>>()?;
            }
            return Ok(Run::Float(FloatRun { f, shift, n, map, m, whole, exp, t, class, man }));
        }
        if let Some(&q) = Quant::ALL.iter().find(|q| q.tag() == tag) {
            let blocks = num(p, at)?;
            let tables = q.coded().map(|part| table(p, at, part.alphabet())).collect::<Result<Vec<_>>>()?;
            return Ok(Run::Quant { q, blocks, tables });
        }
        if tag == LANES_TAG {
            let w = byte(p, at)? as usize;
            if !matches!(w, 1 | 2 | 4 | 8) {
                return Err(corrupt());
            }
            let n = num(p, at)?;
            let nib = byte(p, at)?;
            if w < 8 && nib >> w != 0 {
                return Err(corrupt());
            }
            let tables = (0..2 * nib.count_ones()).map(|_| table(p, at, 16)).collect::<Result<_>>()?;
            return Ok(Run::Lanes(LanesRun { w, n, nib, tables }));
        }
        if tag == GRID_TAG {
            let f = Float::of_tag(byte(p, at)?).filter(|f| matches!(f, Float::Bf16 | Float::F16 | Float::F32));
            let f = f.ok_or_else(corrupt)?;
            let (n, start, row, br, bc) = (num(p, at)?, num(p, at)?, num(p, at)?, num(p, at)?, num(p, at)?);
            if row == 0 || br == 0 || bc == 0 || bc > row || row.div_ceil(bc) > MAX_TILES {
                return Err(corrupt());
            }
            let mode = byte(p, at)?;
            if mode > 1 {
                return Err(corrupt());
            }
            let len = num(p, at)?;
            let (grid, k) = if len == 0 {
                (GridValues::Same, k)
            } else {
                if len > MAX_GRID as u64 {
                    return Err(corrupt());
                }
                let len = len as usize;
                let b = p.get(*at..*at + 4 * len).ok_or_else(corrupt)?;
                *at += 4 * len;
                (GridValues::Own(b.chunks(4).map(|c| f32::from_le_bytes(c.try_into().unwrap())).collect()), len)
            };
            if k == 0 {
                return Err(corrupt());
            }
            let (tables, class) = if k <= 16 {
                (vec![table(p, at, k)?], [0; 16])
            } else {
                let hi = table(p, at, k.div_ceil(16))?;
                let (c, class) = classes(p, at)?;
                let mut tables = vec![hi];
                for _ in 0..c {
                    tables.push(table(p, at, 16)?);
                }
                (tables, class)
            };
            return Ok(Run::Grid(GridRun { f, n, start, row, br, bc, zero: mode == 1, grid, k, tables, class }));
        }
        Err(corrupt())
    }

    /// Appends its parameters to `out`.
    pub fn write(&self, out: &mut Vec<u8>) {
        out.push(self.tag());
        match self {
            Run::Raw(n) => varint::put(out, *n),
            Run::Float(r) => {
                out.push(r.shift as u8);
                varint::put(out, r.n);
                out.push(r.m | (r.whole as u8) << 7);
                for &e in &r.map[..r.m as usize] {
                    varint::put(out, e as u64);
                }
                put_table(out, &r.exp);
                out.push(r.t as u8);
                if r.t > 0 {
                    put_classes(out, r.man.len(), &r.class);
                    r.man.iter().for_each(|t| put_table(out, t));
                }
            }
            Run::Quant { blocks, tables, .. } => {
                varint::put(out, *blocks);
                tables.iter().for_each(|t| put_table(out, t));
            }
            Run::Lanes(r) => {
                out.push(r.w as u8);
                varint::put(out, r.n);
                out.push(r.nib);
                r.tables.iter().for_each(|t| put_table(out, t));
            }
            Run::Grid(r) => {
                out.push(r.f.tag());
                for v in [r.n, r.start, r.row, r.br, r.bc] {
                    varint::put(out, v);
                }
                out.push(r.zero as u8);
                match &r.grid {
                    GridValues::Same => out.push(0),
                    GridValues::Own(g) => {
                        varint::put(out, g.len() as u64);
                        g.iter().for_each(|v| out.extend_from_slice(&v.to_le_bytes()));
                    }
                }
                put_table(out, &r.tables[0]);
                if r.k > 16 {
                    put_classes(out, r.tables.len() - 1, &r.class);
                    r.tables[1..].iter().for_each(|t| put_table(out, t));
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
            Part::Raw(_) => {
                for b in 0..blocks {
                    raw[raw_at..raw_at + n].copy_from_slice(&d[b * bs + at..][..n]);
                    raw_at += n;
                }
            }
            Part::Nib(_) if n < 16 => {
                let s = &mut syms[s_at..s_at + blocks * n * 2];
                for b in 0..blocks {
                    let (src, g) = (&d[b * bs + at..][..n], &mut s[b * n * 2..][..n * 2]);
                    for i in 0..n {
                        g[i] = src[i] & 15;
                        g[n + i] = src[i] >> 4;
                    }
                }
                s_at += blocks * n * 2;
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
            Part::Three256 | Part::Iq1sHigh | Part::Iq1mScales | Part::TopNib(_) | Part::Iq2Xxs => {
                let (k, nr) = (part.syms(), part.raw());
                for b in 0..blocks {
                    let src = &d[b * bs + at..][..n];
                    split_small(*part, src, &mut syms[s_at + b * k..][..k], &mut raw[raw_at..][..nr]);
                    raw_at += nr;
                }
                s_at += blocks * k;
            }
        }
        at += n;
    }
}

fn u16_at(d: &[u8], i: usize) -> u32 {
    u16::from_le_bytes([d[2 * i], d[2 * i + 1]]) as u32
}

fn u32_at(d: &[u8], i: usize) -> u32 {
    u32::from_le_bytes(d[4 * i..4 * i + 4].try_into().unwrap())
}

/// The low 28 bits of number `i` of the 32-bit numbers packed as [`Part::TopNib`]'s raw bytes
/// `r`.
fn low28(r: &[u8], i: usize) -> u32 {
    let mut b = [0u8; 8];
    b[..7].copy_from_slice(&r[7 * (i / 2)..][..7]);
    (u64::from_le_bytes(b) >> (28 * (i % 2))) as u32 & 0x0fff_ffff
}

/// The low 28 bits of numbers `a` and `b` packed into `r` (7 bytes), [`low28`]'s.
fn put28(a: u32, b: u32, r: &mut [u8]) {
    let v = (a & 0x0fff_ffff) as u64 | ((b & 0x0fff_ffff) as u64) << 28;
    r[..7].copy_from_slice(&v.to_le_bytes()[..7]);
}

/// A part of the small parts of a block taken apart: its bytes `src` into its symbols `g` and
/// raw bytes `r`.
fn split_small(part: Part, src: &[u8], g: &mut [u8], r: &mut [u8]) {
    match part {
        Part::Three256 => {
            let (hm, qs) = src.split_at(32);
            for h in 0..2 {
                for m in 0..4 {
                    for l in 0..32 {
                        g[128 * h + 32 * m + l] = qs[32 * h + l] >> (2 * m) & 3 | (hm[l] >> (4 * h + m) & 1) << 2;
                    }
                }
            }
        }
        Part::Iq1sHigh => {
            r.fill(0);
            for i in 0..8 {
                let w = u16_at(src, i);
                g[i] = (w >> 12 & 7) as u8;
                let lo = (w & 0xfff) << (12 * (i % 2));
                for k in 0..3 {
                    r[3 * (i / 2) + k] |= (lo >> (8 * k)) as u8;
                }
                r[12] |= ((w >> 15) as u8) << i;
            }
        }
        Part::Iq1mScales => {
            for i in 0..4 {
                let w = u16_at(src, i);
                for k in 0..4 {
                    g[4 * i + k] = (w >> (3 * k) & 7) as u8;
                }
            }
            for (j, v) in r.iter_mut().enumerate() {
                *v = (u16_at(src, 2 * j) >> 12 | u16_at(src, 2 * j + 1) >> 12 << 4) as u8;
            }
        }
        Part::TopNib(n) => {
            for (i, v) in g.iter_mut().enumerate() {
                *v = (u32_at(src, i) >> 28) as u8;
            }
            for j in 0..n / 8 {
                put28(u32_at(src, 2 * j), u32_at(src, 2 * j + 1), &mut r[7 * j..]);
            }
        }
        Part::Iq2Xxs => {
            for i in 0..8 {
                g[i] = (u32_at(src, 2 * i + 1) >> 28) as u8;
                r[4 * i..4 * i + 4].copy_from_slice(&src[8 * i..8 * i + 4]);
            }
            for j in 0..4 {
                put28(u32_at(src, 4 * j + 1), u32_at(src, 4 * j + 3), &mut r[32 + 7 * j..]);
            }
        }
        _ => unreachable!(),
    }
}

/// [`split_small`] undone: the part's bytes `o` from its symbols `g` and raw bytes `r`.
fn join_small(part: Part, g: &[u8], r: &[u8], o: &mut [u8]) {
    match part {
        Part::Three256 => {
            let (hm, qs) = o.split_at_mut(32);
            hm.fill(0);
            qs.fill(0);
            for h in 0..2 {
                for m in 0..4 {
                    for l in 0..32 {
                        let v = g[128 * h + 32 * m + l];
                        qs[32 * h + l] |= (v & 3) << (2 * m);
                        hm[l] |= (v >> 2) << (4 * h + m);
                    }
                }
            }
        }
        Part::Iq1sHigh => {
            for i in 0..8 {
                let p = &r[3 * (i / 2)..][..3];
                let lo = (p[0] as u32 | (p[1] as u32) << 8 | (p[2] as u32) << 16) >> (12 * (i % 2)) & 0xfff;
                let w = lo | (g[i] as u32) << 12 | ((r[12] >> i & 1) as u32) << 15;
                o[2 * i..2 * i + 2].copy_from_slice(&(w as u16).to_le_bytes());
            }
        }
        Part::Iq1mScales => {
            for i in 0..4 {
                let top = (r[i / 2] >> (4 * (i % 2)) & 15) as u32;
                let w = (0..4).fold(top << 12, |w, k| w | (g[4 * i + k] as u32) << (3 * k));
                o[2 * i..2 * i + 2].copy_from_slice(&(w as u16).to_le_bytes());
            }
        }
        Part::TopNib(_) => {
            for (i, &v) in g.iter().enumerate() {
                o[4 * i..4 * i + 4].copy_from_slice(&(low28(r, i) | (v as u32) << 28).to_le_bytes());
            }
        }
        Part::Iq2Xxs => {
            for i in 0..8 {
                o[8 * i..8 * i + 4].copy_from_slice(&r[4 * i..4 * i + 4]);
                let w = low28(&r[32..], i) | (g[i] as u32) << 28;
                o[8 * i + 4..8 * i + 8].copy_from_slice(&w.to_le_bytes());
            }
        }
        _ => unreachable!(),
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
    q.coded().map(|p| p.syms()).sum::<usize>() * blocks
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
            Part::Raw(_) => {
                for b in 0..blocks {
                    out[b * bs + at..][..n].copy_from_slice(&raw[raw_at..][..n]);
                    raw_at += n;
                }
            }
            Part::Nib(_) if n < 16 => {
                let s = &syms[s_at..s_at + blocks * n * 2];
                for b in 0..blocks {
                    let (o, g) = (&mut out[b * bs + at..][..n], &s[b * n * 2..][..n * 2]);
                    for i in 0..n {
                        o[i] = g[i] | g[n + i] << 4;
                    }
                }
                s_at += blocks * n * 2;
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
            Part::Three256 | Part::Iq1sHigh | Part::Iq1mScales | Part::TopNib(_) | Part::Iq2Xxs => {
                let (k, nr) = (part.syms(), part.raw());
                for b in 0..blocks {
                    join_small(*part, &syms[s_at + b * k..][..k], &raw[raw_at..][..nr], &mut out[b * bs + at..][..n]);
                    raw_at += nr;
                }
                s_at += blocks * k;
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
        assert_eq!(sizes, [18, 20, 34, 144, 210, 18, 136, 17, 22, 24, 176, 84, 110, 66, 74, 98, 50, 110, 82, 56]);
        assert!(Quant::ALL.iter().all(|&q| quant_syms(q, q.chunk()) <= MAX_CHUNK_SYMS));
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
    fn kept_bits_back() {
        for f in Float::ALL {
            for shift in 0..=f.man_bits() {
                let w = 8 * f.bytes() as u32;
                for v in [0u64, 1, 0x1234_5678_9abc_def0, u64::MAX, 0x8000_0000_0000_0001, 0x7F80_0000] {
                    let v = if w == 64 { v } else { v & ((1 << w) - 1) };
                    let v = v & !((1 << shift) - 1);
                    let e = f.exp(v) as u64;
                    let bits = kept_bits(f, shift, v);
                    assert!(bits < 1 << (1 + f.man_bits() - shift));
                    assert_eq!(kept_float(f, shift, e, bits), v, "{f:?} {shift} {v:x}");
                }
            }
        }
    }

    #[test]
    fn bits() {
        let mut d = vec![0u8; 256];
        let vals: Vec<(u32, u64)> = (1..=53).map(|b| (b, (0x1f_ffff_ffff_ffff ^ b as u64) & ((1 << b) - 1))).collect();
        let mut at = 0;
        for &(b, v) in &vals {
            put_bits(&mut d, at, b, v);
            at += b as usize;
        }
        let mut at = 0;
        for &(b, v) in &vals {
            assert_eq!(get_bits(&d, at, b), v, "{b}");
            at += b as usize;
        }
    }

    /// Rounding to F16 and BF16 at the edges (the values numpy gives).
    #[test]
    fn rounding() {
        let cases = [
            (1.0f32, 0x3c00u64, 0x3f80u64),
            (65504.0, 0x7bff, 0x4780),
            (65519.0, 0x7bff, 0x4780),
            (65520.0, 0x7c00, 0x4780),
            (5.960_464_5e-8, 0x0001, 0x3380),
            (2.980_232_2e-8, 0x0000, 0x3300),
            (2.980_233e-8, 0x0001, 0x3300),
            (-6.097_555e-5, 0x83ff, 0xb880),
            (1.000_976_6, 0x3c01, 0x3f80),
            (1.000_488_3, 0x3c00, 0x3f80),
            (1.001_464_8, 0x3c02, 0x3f80),
            (1.003_906_3, 0x3c04, 0x3f80),
            (1.011_718_8, 0x3c0c, 0x3f82),
            (3.0e38, 0x7c00, 0x7f62),
        ];
        for (x, h, b) in cases {
            assert_eq!(round_f32(Float::F16, x), h, "{x}");
            assert_eq!(round_f32(Float::Bf16, x), b, "{x}");
        }
        assert_eq!(round_f32(Float::Bf16, f32::NAN), 0x7fc0);
        assert_eq!(round_f32(Float::F32, -f32::NAN), 0x7fc0_0000);
    }

    #[test]
    fn runs_back() {
        let t = Table::of_counts(&[3, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]);
        let t4 = Table::of_counts(&[3, 1, 2, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        let runs = [
            Run::Raw(77),
            Run::Float(FloatRun {
                f: Float::F64,
                shift: 3,
                n: 100_000,
                map: [1023, 1022, 1021, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
                m: 3,
                whole: true,
                exp: t,
                t: 2,
                class: [0, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1],
                man: vec![t4, t4],
            }),
            Run::Lanes(LanesRun { w: 4, n: 9, nib: 0b1001, tables: vec![t; 4] }),
            Run::Grid(GridRun {
                f: Float::Bf16,
                n: 4096,
                start: 0,
                row: 1024,
                br: 1,
                bc: 16,
                zero: false,
                grid: GridValues::Own(vec![0.0, 0.5, 1.0, -1.0]),
                k: 4,
                tables: vec![t4],
                class: [0; 16],
            }),
            Run::Grid(GridRun {
                f: Float::F16,
                n: 4096,
                start: 100,
                row: 64,
                br: 32,
                bc: 1,
                zero: true,
                grid: GridValues::Same,
                k: 4,
                tables: vec![t4],
                class: [0; 16],
            }),
        ];
        let mut p = Vec::new();
        runs.iter().for_each(|r| r.write(&mut p));
        let mut at = 0;
        let mut k = 0;
        for r in &runs {
            let got = Run::read(&p, &mut at, k).unwrap();
            if let Run::Grid(g) = &got {
                k = g.k;
            }
            assert_eq!(&got, r);
        }
        assert_eq!(at, p.len());
    }
}
