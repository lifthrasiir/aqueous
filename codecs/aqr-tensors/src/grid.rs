//! Tensors of floats that are the values of a grid scaled by tiles (FORMAT.md §4.5): weights of a
//! lower precision converted to BF16, F16 or F32 (NVFP4's, MXFP4's, FP8's of blocks, NF4's,
//! GPTQ's and AWQ's, INT8's of rows). A sample of the tensor tells first whether any structure of
//! the kind is there ([`detect`]): values fewer than they would be in blocks of rows or of
//! columns, or a mantissa of few values; only then are the grids tried on tiles of the sample,
//! and one taken where it would come to less than the floats' coding. A run of a grid
//! ([`encode`]) fits each tile it is over (the whole tile, of the tensor), its scale the one of
//! the fewest bits that gives every element exactly; a tile that fits no scale is escaped.

use std::collections::BTreeMap;

use crate::layout::{ESCAPED, Float, GridRun, GridValues, grid_value};
use crate::rans::{Encoder, LANES, Table};

/// A grid: its values, in the order of their indices.
#[derive(Debug)]
pub struct Grid {
    pub name: &'static str,
    pub values: Vec<f32>,
    /// Whether its values are less a zero point (UINT4 of GPTQ and AWQ).
    pub zero: bool,
    /// The values but -0 by value, with their indices.
    sorted: Vec<(f64, u8)>,
    /// The midpoints between the values of `sorted`: the index of the nearest to `x` that of
    /// the count of them below it.
    mids: Vec<f64>,
    /// The magnitudes not 0, the largest first.
    mags: Vec<f64>,
    /// Of each index, how far its value is from the nearest other.
    gap: Vec<f64>,
    plus0: Option<u8>,
    minus0: Option<u8>,
}

impl Grid {
    fn new(name: &'static str, values: Vec<f32>, zero: bool) -> Grid {
        let mut sorted: Vec<(f64, u8)> = (values.iter().enumerate())
            .filter(|(_, v)| !(**v == 0.0 && v.is_sign_negative()))
            .map(|(i, &v)| (v as f64, i as u8))
            .collect();
        sorted.sort_by(|a, b| a.0.total_cmp(&b.0));
        sorted.dedup_by(|a, b| a.0 == b.0);
        let mut mags: Vec<f64> = values.iter().filter(|v| **v != 0.0).map(|v| v.abs() as f64).collect();
        mags.sort_by(|a, b| b.total_cmp(a));
        mags.dedup();
        let find = |neg: bool| values.iter().position(|&v| v == 0.0 && v.is_sign_negative() == neg).map(|i| i as u8);
        let (plus0, minus0) = (find(false), find(true));
        let mids = sorted.windows(2).map(|w| (w[0].0 + w[1].0) / 2.0).collect();
        let gap = (values.iter())
            .map(|&v| {
                values.iter().filter(|&&w| w != v).map(|&w| (w as f64 - v as f64).abs()).fold(f64::INFINITY, f64::min)
            })
            .collect();
        Grid { name, values, zero, sorted, mids, mags, gap, plus0, minus0 }
    }

    /// The index of the value nearest `x` (not a zero's sign; of a tie, the lower).
    #[inline]
    fn nearest(&self, x: f64) -> u8 {
        let p = if self.mids.len() <= 16 {
            self.mids.iter().map(|&m| (m < x) as usize).sum()
        } else {
            self.mids.partition_point(|&m| m < x)
        };
        self.sorted[p].1
    }

    /// The index of a zero of `x`'s sign.
    fn zero_of(&self, negative: bool) -> Option<u8> {
        if negative { self.minus0 } else { self.plus0 }
    }
}

/// The values of FP4's E2M1 by its codes.
fn e2m1() -> Vec<f32> {
    let m = [0.0, 0.5, 1.0, 1.5, 2.0, 3.0, 4.0, 6.0];
    (0..16).map(|c| if c < 8 { m[c] } else { -m[c - 8] }).collect()
}

/// The values of a float of `m` bits of mantissa and bias `bias` (no infinities)
/// of magnitudes' codes below `codes`, index `2c + sign` that of code `c`.
fn minifloat(m: u32, bias: i32, codes: u32) -> Vec<f32> {
    let mut v = Vec::new();
    for c in 0..codes {
        let (ex, ma) = ((c >> m) as i32, (c & ((1 << m) - 1)) as f64 / (1 << m) as f64);
        let x = if ex == 0 { ma * 2f64.powi(1 - bias) } else { (1.0 + ma) * 2f64.powi(ex - bias) };
        v.push(x as f32);
        v.push(-x as f32);
    }
    v
}

/// The grids tried.
pub fn grids() -> &'static [Grid] {
    static GRIDS: std::sync::OnceLock<Vec<Grid>> = std::sync::OnceLock::new();
    GRIDS.get_or_init(|| {
        // (bitsandbytes' NF4)
        let nf4 = [
            -1.0,
            -0.696_192_8,
            -0.525_073_05,
            -0.394_917_5,
            -0.284_441_38,
            -0.184_773_43,
            -0.091_050_036,
            0.0,
            0.079_580_3,
            0.160_930_2,
            0.246_112_3,
            0.337_915_24,
            0.440_709_83,
            0.562_617,
            0.722_956_84,
            1.0,
        ];
        vec![
            Grid::new("e2m1", e2m1(), false),
            Grid::new("nf4", nf4.to_vec(), false),
            Grid::new("int4", (0..16i32).map(|c| (c << 28 >> 28) as f32).collect(), false),
            Grid::new("uint4", (0..16).map(|c| c as f32).collect(), true),
            Grid::new("e4m3", minifloat(3, 7, 127), false),
            Grid::new("e5m2", minifloat(2, 15, 124), false),
            Grid::new("e2m3", minifloat(3, 1, 32), false),
            Grid::new("e3m2", minifloat(2, 3, 32), false),
            // (zigzag: 2v for v ≥ 0, -2v - 1 below)
            Grid::new(
                "int8",
                (0..256).map(|q| if q % 2 == 0 { (q / 2) as f32 } else { -((q + 1) / 2) as f32 }).collect(),
                false,
            ),
        ]
    })
}

/// How a tensor is a grid: which, and its tiles.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cfg {
    pub grid: usize,
    pub row: u64,
    pub br: u64,
    pub bc: u64,
}

/// The value of float `v` (of BF16, F16, F32), and half its ulp.
#[inline]
fn value(f: Float, v: u64) -> (f64, f64) {
    let (m, bias) = (f.man_bits() as i64, (1i64 << (f.exp_bits() - 1)) - 1);
    let e = f.exp(v) as i64;
    let x = match f {
        Float::Bf16 => f32::from_bits((v as u32) << 16) as f64,
        Float::F16 => {
            let mant = v & 0x3ff;
            let a = if e == 0 {
                mant as f64 * (1.0 / 16_777_216.0)
            } else {
                f64::from_bits(((e - 15 + 1023) as u64) << 52 | mant << 42)
            };
            if v & 0x8000 != 0 { -a } else { a }
        }
        _ => f32::from_bits(v as u32) as f64,
    };
    // (half an ulp: 2^(e - bias - m - 1), of the least exponent for a subnormal)
    let half = f64::from_bits(((e.max(1) - bias - m - 1 + 1023) as u64) << 52);
    (x, half)
}

/// The f32 of the fewest bits of mantissa in `[lo, hi]` (0 < lo), if any is: of the f32s in it,
/// the least and the most share their high bits; the one of those bits and a 1 after them.
fn shortest(lo: f64, hi: f64) -> Option<f32> {
    if !(lo > 0.0 && lo <= hi && hi < f32::MAX as f64) {
        return None;
    }
    let mut a = lo as f32;
    if (a as f64) < lo {
        a = f32::from_bits(a.to_bits() + 1);
    }
    let mut b = hi as f32;
    if (b as f64) > hi {
        b = f32::from_bits(b.to_bits() - 1);
    }
    if a > b || a < f32::MIN_POSITIVE {
        return None;
    }
    let (x, y) = (a.to_bits(), b.to_bits());
    if x == y {
        return Some(a);
    }
    let h = 31 - (x ^ y).leading_zeros();
    Some(f32::from_bits(y & !((1 << h) - 1)))
}

/// A tile's elements: their bits and values.
struct Tile<'a> {
    f: Float,
    bits: &'a [u64],
    xs: &'a [f64],
    halves: &'a [f64],
}

impl Tile<'_> {
    /// Whether scale `s` gives every element by the indices `qs`.
    fn holds(&self, g: &Grid, s: f32, z: f32, qs: &[u8]) -> bool {
        (self.bits.iter().zip(qs)).all(|(&b, &q)| grid_value(self.f, g.values[q as usize], s, z, g.zero) == b)
    }

    /// The indices of the elements at about scale `s` (and zero point `z`), None where one is
    /// further from its value than an ulp or two.
    fn indices(&self, g: &Grid, s: f64, z: f64, qs: &mut [u8]) -> Option<()> {
        for (i, out) in qs.iter_mut().enumerate().take(self.xs.len()) {
            let x = self.xs[i];
            *out = if x == 0.0 && !g.zero {
                g.zero_of(self.bits[i] >> (8 * self.f.bytes() - 1) == 1)?
            } else {
                let q = g.nearest(x / s + z);
                let y = (g.values[q as usize] as f64 - z) * s;
                if (y - x).abs() > 6.0 * self.halves[i] + 1e-300 {
                    return None;
                }
                q
            };
        }
        Some(())
    }

    /// The scales for the indices `qs` (and zero point `z`) that every element is in the range of
    /// scales that round to it, about: the one of the fewest bits, then others further in (the
    /// range's ends may round the other way, of ties). The elements whose value at about scale
    /// `s0` is as near another index's as an ulp or so (of a grid finer there than the floats)
    /// are left out of the range, where any are not.
    fn scales(&self, g: &Grid, z: f64, s0: f64, qs: &[u8]) -> Vec<f32> {
        let range = |all: bool| {
            let (mut lo, mut hi) = (0f64, f64::INFINITY);
            for (i, &q) in qs.iter().enumerate().take(self.xs.len()) {
                let q = q as usize;
                let d = g.values[q] as f64 - z;
                let (x, h) = (self.xs[i], self.halves[i]);
                if d == 0.0 || !all && g.gap[q] * s0 < 4.0 * h {
                    continue;
                }
                let (a, b) = ((x - h) / d, (x + h) / d);
                let (a, b) = if d > 0.0 { (a, b) } else { (b, a) };
                lo = lo.max(a);
                hi = hi.min(b);
            }
            (lo, hi)
        };
        let (mut lo, mut hi) = range(false);
        if hi == f64::INFINITY {
            (lo, hi) = range(true);
        }
        if hi == f64::INFINITY {
            return vec![1.0];
        }
        let lo = lo.max(f32::MIN_POSITIVE as f64);
        let w = hi - lo;
        let mut out: Vec<f32> =
            [shortest(lo, hi), shortest(lo + w / 8.0, hi - w / 8.0)].into_iter().flatten().collect();
        if lo <= hi {
            out.push(((lo + hi) / 2.0) as f32);
        }
        out
    }

    /// Whether one of `scales` gives the tile (indices of each found again), and which.
    fn first(&self, g: &Grid, z: f64, scales: &[f32], qs: &mut [u8]) -> Option<f32> {
        // (the indices as they are first: of the scale they were found at, most often the same)
        let holds = |s: f32, qs: &mut [u8]| {
            self.holds(g, s, z as f32, qs)
                || self.indices(g, s as f64, z, qs).is_some() && self.holds(g, s, z as f32, qs)
        };
        scales.iter().copied().find(|&s| holds(s, qs))
    }

    /// The scale (and zero point) of the tile in grid `g`, its indices put in `qs`; `hints` the
    /// scales to try first (of tiles near it).
    fn fit(&self, g: &Grid, hints: &[(f32, f32)], qs: &mut [u8]) -> Option<(f32, f32)> {
        if self.xs.iter().any(|x| !x.is_finite()) {
            return None;
        }
        // (a hint tried on the largest element first: most do not give it)
        let top = (0..self.xs.len()).max_by(|&a, &b| self.xs[a].abs().total_cmp(&self.xs[b].abs())).unwrap_or(0);
        for &(s, z) in hints {
            let q = g.nearest(self.xs[top] / s as f64 + z as f64);
            if s > 0.0
                && grid_value(self.f, g.values[q as usize], s, z, g.zero) == self.bits[top]
                && self.indices(g, s as f64, z as f64, qs).is_some()
                && self.holds(g, s, z, qs)
            {
                return Some((s, z));
            }
        }
        if g.zero {
            return self.fit_zero(g, qs);
        }
        let amax = self.xs.iter().fold(0f64, |a, x| a.max(x.abs()));
        if amax == 0.0 {
            return self.indices(g, 1.0, 0.0, qs).and_then(|_| self.holds(g, 1.0, 0.0, qs).then_some((1.0, 0.0)));
        }
        for &m in g.mags.iter().take(8) {
            let s0 = amax / m;
            if self.indices(g, s0, 0.0, qs).is_none() {
                continue;
            }
            let scales = self.scales(g, 0.0, s0, qs);
            if let Some(s) = self.first(g, 0.0, &scales, qs) {
                return Some((s, 0.0));
            }
        }
        None
    }

    /// [`Tile::fit`] of a grid of a zero point: integers from 0, the least and the most of the
    /// tile their ends.
    fn fit_zero(&self, g: &Grid, qs: &mut [u8]) -> Option<(f32, f32)> {
        let (lo, hi) = self.xs.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), &x| (a.min(x), b.max(x)));
        let top = (g.values.len() - 1) as f64;
        if hi == lo {
            let z = -lo as f32;
            qs.fill(0);
            return self.holds(g, 1.0, z, qs).then_some((1.0, z));
        }
        let s0 = (hi - lo) / top;
        let z0 = (-lo / s0).round();
        for z in [z0, z0 - 1.0, z0 + 1.0] {
            if self.indices(g, s0, z, qs).is_none() {
                continue;
            }
            let scales = self.scales(g, z, s0, qs);
            if let Some(s) = self.first(g, z, &scales, qs) {
                return Some((s, z as f32));
            }
        }
        None
    }
}

/// The elements of a tensor of `f` and a tile of them: rows of `row`, the tile of rows
/// `r0..r1` and columns `c0..c1`, gathered into the buffers.
struct Gather {
    bits: Vec<u64>,
    xs: Vec<f64>,
    halves: Vec<f64>,
}

impl Gather {
    fn new() -> Gather {
        Gather { bits: Vec::new(), xs: Vec::new(), halves: Vec::new() }
    }

    #[allow(clippy::too_many_arguments)]
    fn tile(&mut self, f: Float, d: &[u8], row: u64, r0: u64, r1: u64, c0: u64, c1: u64) -> Tile<'_> {
        let fb = f.bytes();
        self.bits.clear();
        self.xs.clear();
        self.halves.clear();
        for r in r0..r1 {
            let at = ((r * row + c0) as usize) * fb;
            for c in 0..(c1 - c0) as usize {
                let mut b = [0u8; 8];
                b[..fb].copy_from_slice(&d[at + c * fb..at + (c + 1) * fb]);
                let v = u64::from_le_bytes(b);
                let (x, h) = value(f, v);
                self.bits.push(v);
                self.xs.push(x);
                self.halves.push(h);
            }
        }
        Tile { f, bits: &self.bits, xs: &self.xs, halves: &self.halves }
    }
}

fn entropy_of(counts: &[u64]) -> f64 {
    let t: u64 = counts.iter().sum();
    if t == 0 {
        return 0.0;
    }
    let t = t as f64;
    counts.iter().filter(|&&c| c > 0).map(|&c| c as f64 * (t / c as f64).log2()).sum::<f64>() / t
}

/// The counts of the values of `v`, sorted.
fn runs(v: &[u64]) -> Vec<u64> {
    v.chunk_by(|a, b| a == b).map(|r| r.len() as u64).collect()
}

/// Of the values counted, the distinct ones a block of `b` of them would have.
fn expected_distinct(counts: &[u64], b: u64) -> f64 {
    let t: u64 = counts.iter().sum();
    counts.iter().map(|&c| 1.0 - (1.0 - c as f64 / t as f64).powi(b as i32)).sum()
}

/// The mean of the distinct values of blocks of `b`, each the values `value(k, j)` of block
/// `k` (of `blocks`), `j` from 0 to `b`.
fn distinct(blocks: u64, b: u64, value: impl Fn(u64, u64) -> u64) -> f64 {
    let mut buf = Vec::with_capacity(b as usize);
    let mut sum = 0usize;
    for k in 0..blocks {
        buf.clear();
        buf.extend((0..b).map(|j| value(k, j)));
        buf.sort_unstable();
        sum += 1 + buf.windows(2).filter(|w| w[0] != w[1]).count();
    }
    if blocks > 0 { sum as f64 / blocks as f64 } else { 0.0 }
}

/// Elements of a tensor at least, for a grid to be looked for.
pub const MIN_ELEMENTS: u64 = 1 << 15;

/// Whether tensor `d` of `f` (rows of `row`, 0 where not known) is the values of a grid in tiles,
/// and of which, by a sample of it, comparing the bits it comes to with those the floats' coding
/// would take (about, of the sample). None where no structure of a grid is seen, or none would
/// come to less.
pub fn detect(f: Float, d: &[u8], row: u64) -> Option<Cfg> {
    if !matches!(f, Float::Bf16 | Float::F16 | Float::F32) {
        return None;
    }
    let fb = f.bytes();
    let n = (d.len() / fb) as u64;
    if n < MIN_ELEMENTS {
        return None;
    }
    let row = if row == 0 || row > n { n } else { row };
    let rows = n / row;
    let get = |i: u64| {
        let mut b = [0u8; 8];
        b[..fb].copy_from_slice(&d[i as usize * fb..(i as usize + 1) * fb]);
        u64::from_le_bytes(b)
    };
    let sign = 1u64 << (8 * fb - 1);
    let mag = |i: u64| get(i) & (sign - 1);
    // (the sample: windows along the rows, and blocks down the columns)
    let (win, wins) = (1024u64, 8u64);
    let starts: Vec<u64> = (0..wins).map(|k| (n - win) / (wins - 1) * k).collect();
    let mmask = (1u64 << f.man_bits()) - 1;
    let mut mags: Vec<u64> = starts.iter().flat_map(|&s| s..s + win).map(mag).collect();
    let tz =
        mags.iter().map(|v| v & mmask).filter(|&m| m != 0).map(|m| m.trailing_zeros()).min().unwrap_or(f.man_bits());
    let mut exps = vec![0u64; 1 << f.exp_bits()];
    mags.iter().for_each(|&v| exps[f.exp(v) as usize] += 1);
    let mut mans: Vec<u64> = mags.iter().map(|v| v & mmask).collect();
    mans.sort_unstable();
    mags.sort_unstable();
    let (counts, mans) = (runs(&mags), runs(&mans));
    let mut seen = false;
    for b in [16u64, 128] {
        let per = win / b;
        let got = distinct(wins * per, b, |k, j| mag(starts[(k / per) as usize] + k % per * b + j));
        if got < 0.85 * expected_distinct(&counts, b) {
            seen = true;
        }
    }
    if rows >= 64 && row >= 16 {
        let cols = 64.min(row);
        let got = distinct(wins * cols, 16, |k, j| mag(((rows - 16) / (wins - 1) * (k / cols) + j) * row + k % cols));
        if got < 0.85 * expected_distinct(&counts, 16) {
            seen = true;
        }
    }
    // (a mantissa of few values, or of few bits: of a lower precision, scaled)
    // (of no more bits than the sample can tell: about the log of its size)
    let man_bits = entropy_of(&mans);
    let room = ((f.man_bits() - tz) as f64).min(((wins * win) as f64).log2() - 2.0);
    if man_bits < room - 1.0 || f.man_bits() - tz <= 3 {
        seen = true;
    }
    if !seen {
        return None;
    }
    // the floats' coding, about: the exponents' entropy and the bits kept
    let float_bits = entropy_of(&exps) + (1 + f.man_bits() - tz) as f64;
    // the grids on tiles of the sample, of each shape
    let mut shapes = vec![];
    for bc in [16u64, 32, 64, 128, 256] {
        if bc <= row {
            shapes.push((1, bc));
        }
    }
    shapes.push((1, row));
    for br in [16u64, 32, 64, 128] {
        if br <= rows {
            shapes.push((br, 1));
        }
    }
    if rows >= 128 && row >= 128 {
        shapes.push((128, 128));
    }
    let mut ga = Gather::new();
    let mut qs = Vec::new();
    // (the bits an element comes to, about, of grid `gi` in tiles of `br` by `bc`: on `bands`
    // bands of tiles spread over the rows, of about `budget` elements each; None where fewer
    // than half the elements fit)
    let mut trial = |gi: usize, br: u64, bc: u64, budget: u64, bands: u64| -> Option<f64> {
        let g = &grids()[gi];
        if row.div_ceil(bc) > crate::layout::MAX_TILES {
            return None;
        }
        let cols = (budget / br).max(bc).min(row) / bc * bc;
        let bands = bands.min(rows / br);
        if bands == 0 || cols == 0 {
            return None;
        }
        let (mut ok, mut all, mut esc, mut tiles) = (0u64, 0u64, 0u64, 0u64);
        let mut hq = [0u64; 256];
        let mut hs: BTreeMap<(u32, u32), u64> = BTreeMap::new();
        for k in 0..bands {
            let r0 = (rows / br - 1) / bands * k * br;
            let mut hint: Option<(f32, f32)> = None;
            for c0 in (0..cols).step_by(bc as usize) {
                let c1 = (c0 + bc).min(row);
                let t = ga.tile(f, d, row, r0, r0 + br, c0, c1);
                qs.resize(t.xs.len(), 0);
                let hints: &[(f32, f32)] = match &hint {
                    Some(h) => std::slice::from_ref(h),
                    None => &[],
                };
                let e = t.xs.len() as u64;
                all += e;
                tiles += 1;
                match t.fit(g, hints, &mut qs) {
                    Some(sz) => {
                        ok += e;
                        hint = Some(sz);
                        qs.iter().for_each(|&q| hq[q as usize] += 1);
                        *hs.entry((sz.0.to_bits(), sz.1.to_bits())).or_default() += 1;
                    }
                    None => {
                        esc += e;
                        // (a grid that fits too few of the first tiles: not of these tiles)
                        if esc > 4 * ok.max(e) {
                            return None;
                        }
                    }
                }
            }
        }
        if ok < all / 2 {
            return None;
        }
        let scale_bits =
            (entropy_of(&hs.values().copied().collect::<Vec<_>>()) + 2.0).min(if g.zero { 64.0 } else { 32.0 });
        let bits = ok as f64 * entropy_of(&hq) + tiles as f64 * scale_bits + esc as f64 * 8.0 * fb as f64;
        Some(bits / all as f64)
    };
    // (every grid and shape on a small sample, the best three of them again on a larger one)
    let mut first: Vec<(f64, Cfg)> = Vec::new();
    for gi in 0..grids().len() {
        for &(br, bc) in &shapes {
            if let Some(per) = trial(gi, br, bc, 1024, if br * bc > 1024 { 1 } else { 2 }) {
                first.push((per, Cfg { grid: gi, row, br, bc }));
            }
        }
    }
    first.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut best: Option<(f64, Cfg)> = None;
    for &(_, cfg) in first.iter().take(3) {
        if let Some(per) = trial(cfg.grid, cfg.br, cfg.bc, 8192, 4)
            && best.is_none_or(|b| per < b.0)
        {
            best = Some((per, cfg));
        }
    }
    let (per, cfg) = best?;
    (per < 0.95 * float_bits).then_some(cfg)
}

/// The classes of symbols of 16 by their counts of `alphabet` symbols each (`counts[s]`): at most
/// 4, the ones that cost the least with a table of about `table_bits` each; their count, the
/// class of each symbol, and the bits they come to.
pub fn classes(counts: &[[u64; 16]; 16], alphabet: usize, table_bits: f64) -> (usize, [u8; 16], f64) {
    let cost = |h: &[u64; 16]| {
        let t: u64 = h.iter().sum();
        if t == 0 {
            return 0.0;
        }
        h[..alphabet].iter().filter(|&&c| c > 0).map(|&c| c as f64 * (t as f64 / c as f64).log2()).sum::<f64>()
    };
    // (greedily merged, from a class for each symbol counted)
    let mut groups: Vec<(Vec<usize>, [u64; 16])> =
        (0..16).filter(|&s| counts[s].iter().any(|&c| c > 0)).map(|s| (vec![s], counts[s])).collect();
    if groups.is_empty() {
        return (1, [0; 16], 0.0);
    }
    let total = |g: &[(Vec<usize>, [u64; 16])]| g.iter().map(|g| cost(&g.1)).sum::<f64>() + g.len() as f64 * table_bits;
    type Groups = Vec<(Vec<usize>, [u64; 16])>;
    let mut best: Option<(f64, Groups)> = None;
    loop {
        if groups.len() <= 4 {
            let t = total(&groups);
            if best.as_ref().is_none_or(|b| t < b.0) {
                best = Some((t, groups.clone()));
            }
        }
        if groups.len() == 1 {
            break;
        }
        let mut pick = (f64::INFINITY, 0, 0);
        for i in 0..groups.len() {
            for j in i + 1..groups.len() {
                let mut h = groups[i].1;
                h.iter_mut().zip(&groups[j].1).for_each(|(a, b)| *a += b);
                let d = cost(&h) - cost(&groups[i].1) - cost(&groups[j].1);
                if d < pick.0 {
                    pick = (d, i, j);
                }
            }
        }
        let (_, i, j) = pick;
        let gj = groups.remove(j);
        groups[i].0.extend(gj.0);
        for s in 0..16 {
            groups[i].1[s] += gj.1[s];
        }
    }
    let (bits, groups) = best.unwrap();
    let mut class = [0u8; 16];
    for (k, g) in groups.iter().enumerate() {
        for &s in &g.0 {
            class[s] = k as u8;
        }
    }
    (groups.len(), class, bits)
}

/// The tables of symbols `a` (of 16) and of `b`'s by the classes of `a`'s: of the high nibbles
/// and of the low ones of a grid's indices, or of the exponents and of the top bits of a float's
/// mantissa. Their counts `counts[a][b]`.
pub fn class_tables(counts: &[[u64; 16]; 16], alphabet: usize) -> (usize, [u8; 16], Vec<Table>, f64) {
    let (c, class, bits) = classes(counts, alphabet, 300.0);
    let mut hs = vec![[0u64; 16]; c];
    for s in 0..16 {
        for v in 0..16 {
            hs[class[s] as usize][v] += counts[s][v];
        }
    }
    // (a class of no symbols counted: a table of the first)
    let tables = hs
        .iter()
        .map(|h| if h.iter().any(|&x| x > 0) { Table::of_counts(h) } else { Table::of_counts(&[1; 16]) })
        .collect();
    (c, class, tables, bits)
}

/// A run of a grid of the elements `from..from + count` of tensor `d` of `f` (FORMAT.md §4.5),
/// of `cfg`: its symbols put to `enc` (its tables after `tables`), its side bytes to `side`. None
/// where it would not come to less than `limit` bits, or too many of its tiles fit no scale.
#[allow(clippy::too_many_arguments)]
pub fn encode(
    cfg: Cfg,
    f: Float,
    d: &[u8],
    from: u64,
    count: u64,
    same: bool,
    limit: f64,
    enc: &mut Encoder,
    tables: &mut Vec<Table>,
    side: &mut Vec<u8>,
) -> Option<GridRun> {
    let g = &grids()[cfg.grid];
    let fb = f.bytes();
    let Cfg { row, br, bc, .. } = cfg;
    let n = (d.len() / fb) as u64;
    let rows = n.div_ceil(row);
    let tiles = row.div_ceil(bc) as usize;
    let k = g.values.len();
    let end = from + count;
    // the indices of the run's elements, and its side bytes, a band of tiles at a time
    let mut q = vec![0u8; count as usize];
    let mut escaped = vec![false; count as usize];
    let s0 = side.len();
    let mut ga = Gather::new();
    let mut qs = Vec::new();
    let mut fits: Vec<Option<(f32, f32)>> = vec![None; tiles];
    let mut above: Vec<Option<(f32, f32)>> = vec![None; tiles];
    let mut band_q: Vec<u8> = Vec::new();
    let mut esc = 0u64;
    let mut seen = vec![u64::MAX; tiles];
    let (first_band, last_band) = (from / row / br, (end - 1) / row / br);
    for band in first_band..=last_band {
        let (r0, r1) = (band * br, ((band + 1) * br).min(rows));
        band_q.resize(((r1 - r0) * row) as usize, 0);
        // (the tiles of the band: of the run's rows, only those it is over)
        let (e0, e1) = (from.max(r0 * row), end.min(r1 * row));
        let (c_lo, c_hi) = if r1 - r0 == 1 { (e0 - r0 * row, e1 - r0 * row) } else { (0, row) };
        let (t_lo, t_hi) = ((c_lo / bc) as usize, (c_hi.div_ceil(bc)) as usize);
        let mut left: Option<(f32, f32)> = None;
        for tc in t_lo..t_hi {
            let (c0, c1) = (tc as u64 * bc, ((tc as u64 + 1) * bc).min(row));
            // (the rows of a tile past the tensor's end: those there are)
            let r1t = r1.min(n.div_ceil(row));
            let t = ga.tile(f, d, row, r0, r1t, c0, c1.min(row));
            qs.resize(t.xs.len(), 0);
            let mut hints = [(0f32, 0f32); 2];
            let mut nh = 0;
            for h in [above[tc], left].into_iter().flatten() {
                hints[nh] = h;
                nh += 1;
            }
            fits[tc] = t.fit(g, &hints[..nh], &mut qs);
            if let Some(sz) = fits[tc] {
                above[tc] = Some(sz);
                left = Some(sz);
            }
            let w = (c1 - c0) as usize;
            for (r, v) in qs.chunks(w).enumerate() {
                band_q[r * row as usize + c0 as usize..][..w].copy_from_slice(v);
            }
        }
        // the run's elements of the band in order: their indices, and side bytes
        let (mut r, mut c) = (e0 / row, e0 % row);
        let mut tc = (c / bc) as usize;
        let mut cin = c % bc;
        for i in e0..e1 {
            let j = (i - from) as usize;
            if seen[tc] != band {
                seen[tc] = band;
                match fits[tc] {
                    Some((s, z)) => {
                        side.extend_from_slice(&s.to_bits().to_le_bytes());
                        if g.zero {
                            side.extend_from_slice(&z.to_le_bytes());
                        }
                    }
                    None => side.extend_from_slice(&ESCAPED.to_le_bytes()),
                }
            }
            if fits[tc].is_some() {
                q[j] = band_q[((r - r0) * row + c) as usize];
            } else {
                escaped[j] = true;
                esc += 1;
                side.extend_from_slice(&d[i as usize * fb..(i as usize + 1) * fb]);
            }
            c += 1;
            cin += 1;
            if c == row {
                (r, c, tc, cin) = (r + 1, 0, 0, 0);
            } else if cin == bc {
                (tc, cin) = (tc + 1, 0);
            }
        }
    }
    // (too many escaped: the floats' coding is better)
    if esc * 5 > count {
        side.truncate(s0);
        return None;
    }
    // the tables: of the indices, or of their high nibbles and of the low ones by classes
    let mut counts = [[0u64; 16]; 16];
    for (j, &v) in q.iter().enumerate() {
        if !escaped[j] {
            counts[(v >> 4) as usize][(v & 15) as usize] += 1;
        }
    }
    // (an escaped element's index: the commonest's)
    let common = (0..256).max_by_key(|&v| counts[v >> 4][v & 15]).unwrap() as u8;
    counts[(common >> 4) as usize][(common & 15) as usize] += esc;
    for (j, v) in q.iter_mut().enumerate() {
        if escaped[j] {
            *v = common;
        }
    }
    let first = tables.len() as u32;
    let (ts, class, sym_bits) = if k <= 16 {
        let h: [u64; 16] = std::array::from_fn(|v| counts[0][v]);
        let t = Table::of_counts(&h);
        (vec![t], [0u8; 16], t.cost(&h))
    } else {
        let hi: [u64; 16] = std::array::from_fn(|a| counts[a].iter().sum());
        let th = Table::of_counts(&hi);
        let (_, class, lo, bits) = class_tables(&counts, 16);
        let mut ts = vec![th];
        ts.extend(lo);
        (ts, class, th.cost(&hi) + bits)
    };
    let bits = sym_bits + 8.0 * (side.len() - s0) as f64;
    if bits >= limit {
        side.truncate(s0);
        return None;
    }
    tables.extend_from_slice(&ts);
    let c = ts.len() - 1;
    // the symbols, in chunks
    let mut sa = vec![0u8; crate::layout::FLOAT_CHUNK];
    let mut sb: Vec<Vec<u8>> = vec![Vec::new(); c.max(1)];
    for chunk in q.chunks(crate::layout::FLOAT_CHUNK) {
        let e = chunk.len();
        sb.iter_mut().for_each(|b| b.clear());
        for (i, &v) in chunk.iter().enumerate() {
            if k <= 16 {
                sa[i] = v;
            } else {
                sa[i] = v >> 4;
                sb[class[(v >> 4) as usize] as usize].push(v & 15);
            }
        }
        put(enc, first, &sa[..e]);
        if k > 16 {
            for (kk, b) in sb.iter().enumerate().take(c) {
                put(enc, first + 1 + kk as u32, b);
            }
        }
    }
    Some(GridRun {
        f,
        n: count,
        start: from,
        row,
        br,
        bc,
        zero: g.zero,
        grid: if same { GridValues::Same } else { GridValues::Own(g.values.clone()) },
        k,
        tables: ts,
        class,
    })
}

/// Symbols `s` of table `t` as groups of 32, the last perhaps fewer.
pub fn put(enc: &mut Encoder, t: u32, s: &[u8]) {
    let full = s.len() / LANES * LANES;
    enc.groups(t, &s[..full]);
    if s.len() > full {
        enc.group(t, &s[full..]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortest_scales() {
        assert_eq!(shortest(0.9, 1.1), Some(1.0));
        assert_eq!(shortest(1.3, 1.4), Some(1.375));
        assert_eq!(shortest(3.0, 3.0), Some(3.0));
        assert_eq!(shortest(1.1, 1.0), None);
    }

    #[test]
    fn grids_hold() {
        for g in grids() {
            assert!(g.values.len() <= 256, "{}", g.name);
            for (q, &v) in g.values.iter().enumerate() {
                if v != 0.0 {
                    assert_eq!(g.values[g.nearest(v as f64) as usize], v, "{} {q}", g.name);
                }
            }
        }
        assert_eq!(grids()[4].values.len(), 254);
        assert_eq!(grids()[4].values[2 * 0x7e], 448.0);
    }

    /// Tiles of BF16 made of a grid times a scale (of 4 bits more than BF16's): fitted, and
    /// the elements given back exactly.
    #[test]
    fn fits() {
        let mut r = aqr_common::rng::Rng::new(9);
        for (gi, g) in grids().iter().enumerate() {
            let k = g.values.len() as u64;
            for f in [Float::Bf16, Float::F16, Float::F32] {
                let mut fitted = 0;
                for _ in 0..50 {
                    let s = (1.0 + r.below(4096) as f32 / 4096.0) * 2f32.powi(r.below(8) as i32 - 12);
                    let z = if g.zero { r.below(16) as f32 } else { 0.0 };
                    let mut q: Vec<u8> = (0..32).map(|_| r.below(k) as u8).collect();
                    // (the tile's largest the grid's, as of quantising by the largest; of a zero
                    // point, its ends those of the grid, as of quantising between its least and
                    // its most)
                    let top = (0..k as usize).max_by(|&a, &b| g.values[a].abs().total_cmp(&g.values[b].abs())).unwrap();
                    (q[0], q[1]) = if g.zero { (0, (k - 1) as u8) } else { (q[0], top as u8) };
                    let bits: Vec<u64> = q.iter().map(|&q| grid_value(f, g.values[q as usize], s, z, g.zero)).collect();
                    let vals: Vec<(f64, f64)> = bits.iter().map(|&b| value(f, b)).collect();
                    let (xs, halves): (Vec<f64>, Vec<f64>) = vals.into_iter().unzip();
                    let t = Tile { f, bits: &bits, xs: &xs, halves: &halves };
                    let mut qs = vec![0; 32];
                    if let Some((s, z)) = t.fit(g, &[], &mut qs) {
                        assert!(t.holds(g, s, z, &qs), "{gi}");
                        fitted += 1;
                    }
                }
                assert!(fitted >= 40, "{} {f:?}: {fitted} of 50", g.name);
            }
        }
    }
}

#[cfg(test)]
mod rows {
    use super::*;

    /// A row of 512 of INT8 times its scale (the largest over 127), to BF16: fitted (its zeros
    /// +0, of the integers').
    #[test]
    fn int8_row() {
        let mut r = aqr_common::rng::Rng::new(3);
        let g = grids().iter().find(|g| g.name == "int8").unwrap();
        let w: Vec<f32> = (0..512).map(|_| (r.below(1 << 20) as f32 / (1 << 19) as f32 - 1.0) * 0.05).collect();
        let s = w.iter().fold(0f32, |a, x| a.max(x.abs())) / 127.0;
        let bits: Vec<u64> =
            w.iter().map(|&x| crate::layout::round_f32(Float::Bf16, ((x / s).round() + 0.0) * s)).collect();
        let (xs, halves): (Vec<f64>, Vec<f64>) = bits.iter().map(|&b| value(Float::Bf16, b)).unzip();
        let t = Tile { f: Float::Bf16, bits: &bits, xs: &xs, halves: &halves };
        let mut qs = vec![0; 512];
        assert!(t.fit(g, &[], &mut qs).is_some());
    }
}
