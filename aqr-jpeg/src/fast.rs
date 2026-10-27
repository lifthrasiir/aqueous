//! Speed-oriented entropy stage ("P1" model).
//!
//! Per block (raster order, per component; blocks in zigzag order):
//! - number of nonzero AC coefficients, context = average of the above/left counts;
//! - for every AC position up to the last nonzero one: cat = bit length of |x|, context =
//!   (luma/chroma, position, bucket of remaining nonzeros); sign + extra bits raw;
//! - DC: residual of a boundary-continuity prediction from the blocks above / to the left (see
//!   `DcBound`), context = neighbour residual sizes and the disagreement of the two predictions.
//!
//! Static per-file histograms (two pass; see `normalize` for how the tables are kept small),
//! 4-way interleaved 64-bit rANS with 32-bit IO.

use crate::jpeg::{Coefs, Error, Jpeg};
use aqr_common::varint;
use std::time::Instant;

const PROB_BITS: u32 = 12;
const PROB_SCALE: u32 = 1 << PROB_BITS;
const RANS_L: u64 = 1 << 31;

// ---------------------------------------------------------------- contexts / compact indices
const N_NZ: usize = 2 * 64; // alphabet 64
const NREM: usize = 11; // remaining-nonzero classes, see `rem_class`
const N_AC: usize = 2 * 63 * NREM; // alphabet 16
/// DC prediction: boundary continuity with neighbour blocks (true) or MED over the DC grid
const DC_BOUNDARY: bool = true;
/// DC sub-contexts per (component class, neighbour residual bucket): prediction uncertainty
/// (boundary) or AC count bucket (MED)
const DC_SUB: usize = if DC_BOUNDARY { 8 } else { 6 };
const N_DC: usize = 2 * 9 * DC_SUB; // alphabet 16
const NZ_BASE: usize = 0;
const AC_BASE: usize = NZ_BASE + N_NZ * 64;
const DC_BASE: usize = AC_BASE + N_AC * 16;
const N_SYM: usize = DC_BASE + N_DC * 16;

const fn bucket(x: u32, edges: &[u32]) -> u32 {
    let mut n = 0;
    let mut i = 0;
    while i < edges.len() {
        if edges[i] <= x {
            n += 1;
        }
        i += 1;
    }
    n
}

/// Half-logarithmic class of the number of remaining nonzeros (1..=63 -> 0..=10):
/// 1 | 2 | 3 | 4-5 | 6-7 | 8-11 | 12-15 | 16-23 | 24-31 | 32-47 | 48-63.
const fn rem_class(rem: u32) -> usize {
    let b = 32 - rem.leading_zeros();
    let sh = b.saturating_sub(2);
    (2 * b + ((rem >> sh) & 1)) as usize - 3
}

/// AC_IDX[cc][k][rem] = compact index of cat 0 for AC position k (1..63) with `rem` nonzeros left.
static AC_IDX: [[[u32; 64]; 64]; 2] = {
    let mut t = [[[0u32; 64]; 64]; 2];
    let mut cc = 0;
    while cc < 2 {
        let mut k = 1;
        while k < 64 {
            let mut rem = 1;
            while rem < 64 {
                let ctx = (cc * 63 + k - 1) * NREM + rem_class(rem as u32);
                t[cc][k][rem] = (AC_BASE + ctx * 16) as u32;
                rem += 1;
            }
            k += 1;
        }
        cc += 1;
    }
    t
};
static DCN_B: [u8; 64] = {
    let mut t = [0u8; 64];
    let mut i = 0;
    while i < 64 {
        t[i] = bucket(i as u32, &[0, 1, 2, 4, 6, 8, 10, 12]) as u8;
        i += 1;
    }
    t
};
static NZ_B: [u8; 64] = {
    let mut t = [0u8; 64];
    let mut i = 0;
    while i < 64 {
        t[i] = bucket(i as u32, &[0, 2, 5, 10, 20]) as u8;
        i += 1;
    }
    t
};

#[inline(always)]
fn nz_ctx(cc: usize, nza: u32, nzl: u32, has_a: bool, has_l: bool) -> usize {
    let n = has_a as u32 + has_l as u32;
    let s = if has_a { nza } else { 0 } + if has_l { nzl } else { 0 };
    let p = match n {
        2 => s.div_ceil(2),
        1 => s,
        _ => 0,
    };
    NZ_BASE + (cc * 64 + p.min(63) as usize) * 64
}
#[inline(always)]
fn dc_ctx(cc: usize, cat_sum: u32, sub: usize) -> usize {
    DC_BASE + ((cc * 9 + DCN_B[cat_sum.min(63) as usize] as usize) * DC_SUB + sub) * 16
}
static UNC_B: [u8; 128] = {
    let mut t = [0u8; 128];
    let mut i = 0;
    while i < 128 {
        t[i] = bucket(i as u32, &[1, 2, 4, 8, 16, 32, 64]) as u8;
        i += 1;
    }
    t
};

/// natural index -> zigzag index
const ZI: [usize; 64] = {
    let mut t = [0usize; 64];
    let mut z = 0;
    while z < 64 {
        t[crate::jpeg::ZIGZAG[z]] = z;
        z += 1;
    }
    t
};

/// DCT basis scale c(v) = cos(v pi / 16), times 1/sqrt(2) for v = 0, rounded to nearest. A table
/// rather than `f64::cos`, whose result depends on the platform's libm (the weights below are
/// part of the format, and must come out the same in native and Wasm builds).
pub(crate) const CV: [f64; 8] = [
    std::f64::consts::FRAC_1_SQRT_2,
    0.9807852804032304,
    0.9238795325112867,
    0.8314696123025452,
    std::f64::consts::FRAC_1_SQRT_2,
    0.5555702330196023,
    0.38268343236508984,
    0.19509032201612833,
];

/// DC prediction from boundary continuity (Lakhani-style, coefficient domain): the mean of the
/// block's top row / left column must match the bottom row / right column of the neighbour
/// above / to the left. Only column 0 / row 0 coefficients are involved. The sums are in fixed
/// point (wrapping i32); the final scaling, mean and rounding use only basic f32 operations on
/// finite values (no libm), which IEEE 754 makes exact and platform independent.
struct DcBound {
    wa: [i32; 8], // cv[v] q[v][0], v >= 1: this block's column 0 (DC from above)
    wl: [i32; 8], // cv[u] q[0][u], u >= 1: this block's row 0 (DC from left)
    wb: [i32; 8], // cv[v] (-1)^v q[v][0]: bottom boundary (u = 0 term)
    wr: [i32; 8], // cv[u] (-1)^u q[0][u]: right boundary (v = 0 term)
    inv: f32,     // 1 / (cv[0] q[0][0] 2^S)
}
impl DcBound {
    fn new(q: &[u16; 64]) -> Self {
        // q in zigzag order
        let cv = |v: usize| CV[v];
        let sg = |v: usize| if v.is_multiple_of(2) { 1.0 } else { -1.0 };
        let sc = 1024.0;
        let (qc, qr) = (|v: usize| q[ZI[v * 8]] as f64, |u: usize| q[ZI[u]] as f64);
        let mut b = DcBound { wa: [0; 8], wl: [0; 8], wb: [0; 8], wr: [0; 8], inv: 0.0 };
        for v in 0..8 {
            if v > 0 {
                b.wa[v] = (cv(v) * qc(v) * sc).round() as i32;
                b.wl[v] = (cv(v) * qr(v) * sc).round() as i32;
            }
            b.wb[v] = (cv(v) * sg(v) * qc(v) * sc).round() as i32;
            b.wr[v] = (cv(v) * sg(v) * qr(v) * sc).round() as i32;
        }
        b.inv = (1.0 / (cv(0) * q[0].max(1) as f64 * sc)) as f32;
        b
    }
    /// (prediction, uncertainty bucket) for a block whose AC coefficients are known
    #[inline(always)]
    fn pred(&self, b: &[i16; 64], bot_above: i32, rgt_left: i32, has_a: bool, has_l: bool) -> (i32, usize) {
        let (mut sa, mut sl) = (0i32, 0i32);
        for k in 1..8 {
            sa = sa.wrapping_add(self.wa[k].wrapping_mul(b[ZI[k * 8]] as i32));
            sl = sl.wrapping_add(self.wl[k].wrapping_mul(b[ZI[k]] as i32));
        }
        let dca = bot_above.wrapping_sub(sa) as f32 * self.inv;
        let dcl = rgt_left.wrapping_sub(sl) as f32 * self.inv;
        let (pf, unc) = match (has_a, has_l) {
            (true, true) => ((dca + dcl) * 0.5, (dca - dcl).abs()),
            (true, false) => (dca, 0.0),
            (false, true) => (dcl, 0.0),
            _ => (0.0, 0.0),
        };
        // keep residuals within 16 bits (|DC| <= DC_LIMIT)
        let lim = crate::jpeg::DC_LIMIT as f32;
        (pf.round().clamp(-lim, lim) as i32, UNC_B[(unc as u32).min(127) as usize] as usize)
    }
    /// (bottom boundary, right boundary) terms of a complete block, for its neighbours
    #[inline(always)]
    fn bounds(&self, b: &[i16; 64]) -> (i32, i32) {
        let (mut bot, mut rgt) = (0i32, 0i32);
        for k in 0..8 {
            bot = bot.wrapping_add(self.wb[k].wrapping_mul(b[ZI[k * 8]] as i32));
            rgt = rgt.wrapping_add(self.wr[k].wrapping_mul(b[ZI[k]] as i32));
        }
        (bot, rgt)
    }
}
#[inline(always)]
fn med(a: i32, b: i32, c: i32) -> i32 {
    let (mx, mn) = (a.max(b), a.min(b));
    if c >= mx {
        mn
    } else if c <= mn {
        mx
    } else {
        a + b - c
    }
}
#[inline(always)]
fn dc_pred(blocks: &[[i16; 64]], i: usize, bw: usize, bx: usize, by: usize) -> i32 {
    match (by > 0, bx > 0) {
        (true, true) => med(blocks[i - 1][0] as i32, blocks[i - bw][0] as i32, blocks[i - bw - 1][0] as i32),
        (false, true) => blocks[i - 1][0] as i32,
        (true, false) => blocks[i - bw][0] as i32,
        _ => 0,
    }
}

/// cat (bit length of |x|) and the raw payload: (|x| without its top bit) << 1 | sign.
#[inline(always)]
fn split_value(x: i32) -> (u32, u32) {
    let a = x.unsigned_abs();
    let cat = 32 - a.leading_zeros();
    let half = (1u32 << cat) >> 1;
    (cat, ((a & half.wrapping_sub(1)) << 1) | (x < 0) as u32)
}
/// inverse of `split_value` (branchless; cat 0 -> 0)
#[inline(always)]
fn join_value(cat: u32, w: u32) -> i32 {
    let m = ((w >> 1) | ((1u32 << cat) >> 1)) as i32;
    if w & 1 != 0 { -m } else { m }
}

// ---------------------------------------------------------------- encoder

/// LSB-first bit writer. `out.len()` covers the completed bytes; each put stores 8 bytes into
/// the spare capacity (kept >= 16 bytes by `reserve`), so the buffer is never zero-filled.
struct BitWriter {
    acc: u64,
    n: u32,
    out: Vec<u8>,
}
impl BitWriter {
    fn with_capacity(bytes: usize) -> Self {
        BitWriter { acc: 0, n: 0, out: Vec::with_capacity(bytes + 16) }
    }
    /// n <= 25
    #[inline(always)]
    fn put(&mut self, n: u32, v: u32) {
        self.acc |= (v as u64) << self.n;
        self.n += n;
        let len = self.out.len();
        // SAFETY: `reserve` keeps >= 16 bytes of spare capacity; the bytes counted into the
        // length below have just been written
        unsafe {
            (self.out.as_mut_ptr().add(len) as *mut u64).write_unaligned(self.acc.to_le());
            let bytes = self.n >> 3; // n <= 7 + 25, so bytes <= 4
            self.out.set_len(len + bytes as usize);
            self.acc >>= bytes * 8;
        }
        self.n &= 7;
    }
    #[inline(always)]
    fn reserve(&mut self, bytes: usize) {
        if self.out.capacity() - self.out.len() < bytes + 16 {
            self.out.reserve((self.out.capacity()).max(bytes + 16));
        }
    }
    /// The bytes written, the last one zero-padded (readers pad on their own).
    fn finish(mut self) -> Vec<u8> {
        if self.n > 0 {
            self.out.push(self.acc as u8);
        }
        self.out.shrink_to_fit();
        self.out
    }
}

/// The symbols of a stripe, kept compact until the tables are known (see `Walked::expand`):
/// per block the compact indices of its nonzero count and DC symbols and its last nonzero
/// position, and per AC symbol its index within the position's contexts (`AC_SUB`).
struct Sink {
    /// two per block (N_SYM <= 2^16)
    idx: Vec<u16>,
    ni: usize,
    /// one per block (`ni / 2` of them)
    last: Vec<u8>,
    ac: Vec<u8>,
    nc: usize,
    hist: Vec<u32>,
    raw: BitWriter,
}
impl Sink {
    #[inline(always)]
    fn reserve_block(&mut self) {
        if self.ni + 2 > self.idx.len() {
            self.idx.resize((self.idx.len() * 2).max(self.ni + 2), 0);
            self.last.resize(self.idx.len() / 2, 0);
        }
        if self.nc + 63 > self.ac.len() {
            self.ac.resize((self.ac.len() * 2).max(self.nc + 63), 0);
        }
        self.raw.reserve(64 * 2 + 8);
    }
    /// A nonzero count or DC symbol.
    #[inline(always)]
    fn sym(&mut self, idx: usize) {
        debug_assert!(idx < N_SYM);
        // SAFETY: reserve_block keeps room for a whole block; idx < N_SYM by construction
        unsafe {
            *self.idx.get_unchecked_mut(self.ni) = idx as u16;
            *self.hist.get_unchecked_mut(idx) += 1;
        }
        self.ni += 1;
    }
    /// The last nonzero AC position of the block whose symbols are next.
    #[inline(always)]
    fn last(&mut self, k: usize) {
        // SAFETY: reserve_block keeps room for a whole block
        unsafe { *self.last.get_unchecked_mut(self.ni / 2) = k as u8 };
    }
    /// An AC symbol: compact index `idx`, `sub` within its position's contexts.
    #[inline(always)]
    fn ac(&mut self, idx: usize, sub: u32) {
        debug_assert!(idx < N_SYM && sub < 256);
        // SAFETY: reserve_block keeps room for a whole block
        unsafe {
            *self.ac.get_unchecked_mut(self.nc) = sub as u8;
            *self.hist.get_unchecked_mut(idx) += 1;
        }
        self.nc += 1;
    }
}

/// AC_POS[cc][k] = compact index of the first context of AC position k (1..63); a symbol's
/// index is that plus `AC_SUB[rem] + cat`.
static AC_POS: [[u16; 64]; 2] = {
    let mut t = [[0u16; 64]; 2];
    let mut cc = 0;
    while cc < 2 {
        let mut k = 1;
        while k < 64 {
            t[cc][k] = (AC_IDX[cc][k][1] - rem_class(1) as u32 * 16) as u16;
            k += 1;
        }
        cc += 1;
    }
    t
};
static AC_SUB: [u8; 64] = {
    let mut t = [0u8; 64];
    let mut rem = 1;
    while rem < 64 {
        t[rem] = (rem_class(rem as u32) * 16) as u8;
        rem += 1;
    }
    t
};

/// Model the block rows `blocks` (`bw` wide) of a component (one stripe: no context from rows
/// above it).
#[inline(never)]
fn walk_enc(s: &mut Sink, blocks: &[[i16; 64]], masks: &[u64], bw: usize, cc: usize, dcb: &DcBound) {
    let bh = blocks.len() / bw.max(1);
    let mut nzs = vec![0u8; bw * bh];
    let mut dcc = vec![0u8; bw * bh];
    let mut bottom = vec![0i32; bw];
    let mut right = 0i32;
    let ac_idx = &AC_IDX[cc];
    for by in 0..bh {
        for bx in 0..bw {
            let i = by * bw + bx;
            s.reserve_block();
            let (has_a, has_l) = (by > 0, bx > 0);
            let b = &blocks[i];
            let mask = masks[i] & !1;
            let nz = mask.count_ones();
            let (nza, nzl) = (if has_a { nzs[i - bw] } else { 0 }, if has_l { nzs[i - 1] } else { 0 });
            let last = if nz > 0 { 63 - mask.leading_zeros() as usize } else { 0 };
            s.last(last);
            s.sym(nz_ctx(cc, nza as u32, nzl as u32, has_a, has_l) + nz as usize);
            nzs[i] = nz as u8;
            if nz > 0 {
                let mut rem = nz as usize;
                for k in 1..=last {
                    // SAFETY: k < 64, rem in 1..=63 while k <= last
                    let x = unsafe { *b.get_unchecked(k) } as i32;
                    let (cat, payload) = split_value(x);
                    let sub = unsafe { *AC_SUB.get_unchecked(rem) } as u32 + cat;
                    s.ac(unsafe { *ac_idx.get_unchecked(k).get_unchecked(rem) } as usize + cat as usize, sub);
                    s.raw.put(cat, payload);
                    rem -= (x != 0) as usize;
                }
            }
            let (pred, sub) = if DC_BOUNDARY {
                dcb.pred(b, bottom[bx], right, has_a, has_l)
            } else {
                (dc_pred(blocks, i, bw, bx, by), NZ_B[nz as usize] as usize)
            };
            let (cat, payload) = split_value(b[0] as i32 - pred);
            let cs = if has_a { dcc[i - bw] as u32 } else { 0 } + if has_l { dcc[i - 1] as u32 } else { 0 };
            s.sym(dc_ctx(cc, cs, sub) + cat as usize);
            s.raw.put(cat, payload);
            dcc[i] = cat as u8;
            if DC_BOUNDARY {
                (bottom[bx], right) = dcb.bounds(b);
            }
        }
    }
}

// ---------------------------------------------------------------- tables

#[derive(Clone, Copy, Default)]
struct EncSym {
    rcp_freq: u64,
    bias: u32,
    cmpl_freq: u16,
    rcp_shift: u16,
}

pub struct Tables {
    enc: Vec<EncSym>,
    /// decoder: cum | freq << 16
    cf: Vec<u32>,
    /// decoder: coarse slot -> symbol LUT, [(group base / 16) * 64 + (slot >> 6)] = symbol whose
    /// interval contains the bucket's first slot
    lut: Vec<u8>,
}

fn groups() -> impl Iterator<Item = (usize, usize)> {
    (0..N_NZ).map(|c| (NZ_BASE + c * 64, 64)).chain((0..N_AC + N_DC).map(|c| (AC_BASE + c * 16, 16)))
}

/// Largest precision reduction of a group's table: its frequencies sum to PROB_SCALE >> shift.
const MAX_SHIFT: u32 = 8;

/// Normalise counts `h` (total > 0) to sum `total`, every occurring symbol >= 1.
/// Returns false if `total` is too small for the number of occurring symbols.
fn norm_group(h: &[u32], total: u32, f: &mut [u16]) -> bool {
    let tot: u64 = h.iter().map(|&x| x as u64).sum();
    if h.iter().filter(|&&x| x > 0).count() as u32 > total {
        return false;
    }
    let mut s = 0i64;
    let mut best = 0;
    for a in 0..h.len() {
        f[a] = 0;
        if h[a] > 0 {
            f[a] = ((h[a] as u64 * total as u64 / tot) as u16).max(1);
            s += f[a] as i64;
            if h[a] > h[best] {
                best = a;
            }
        }
    }
    let mut d = total as i64 - s;
    if d >= 0 || f[best] as i64 + d >= 1 {
        f[best] = (f[best] as i64 + d) as u16;
    } else {
        while d < 0 {
            for a in 0..h.len() {
                if f[a] > 1 && d < 0 {
                    f[a] -= 1;
                    d += 1;
                }
            }
        }
    }
    true
}

#[inline(always)]
fn gamma_len(v: u32) -> u32 {
    2 * (31 - v.leading_zeros()) + 1
}

/// Best precision shift for counts `h`: (table bits + coded data bits, shift, normalised freqs
/// at that precision).
fn best_shift(h: &[u32], tmp: &mut [u16; 64]) -> (f64, u32) {
    let len = h.len();
    let mut best = (f64::INFINITY, 0u32);
    for sh in 0..=MAX_SHIFT {
        let total = PROB_SCALE >> sh;
        if !norm_group(h, total, &mut tmp[..len]) {
            break;
        }
        let table: u32 = 4 + tmp[..len - 1].iter().map(|&x| gamma_len(x as u32 + 1)).sum::<u32>();
        let data: f64 =
            (0..len).filter(|&a| h[a] > 0).map(|a| h[a] as f64 * (total as f64 / tmp[a] as f64).log2()).sum();
        if table as f64 + data < best.0 {
            best = (table as f64 + data, sh);
        }
    }
    best
}

// Parent groups: a group whose own table is not worth its cost inherits the distribution of
// its parent, which pools related contexts:
//   nz count: (component class, log2 bucket of the neighbours' count)   2 x 8,  alphabet 64
//   AC:       (component class, position) over all remaining-count classes  2 x 63, alphabet 16
//   DC:       (component class, neighbour residual bucket) over all AC-count buckets  2 x 9
const P_NZ: usize = 0;
const P_AC: usize = P_NZ + 2 * 8 * 64;
const P_DC: usize = P_AC + 2 * 63 * 16;
const P_SYM: usize = P_DC + 2 * 9 * 16;

/// Parent group (start in the parent symbol space) of the group starting at `lo`.
#[inline(always)]
fn parent_of(lo: usize) -> usize {
    if lo < AC_BASE {
        let c = (lo - NZ_BASE) / 64;
        let (cc, p) = (c / 64, (c % 64) as u32);
        P_NZ + (cc * 8 + (32 - p.leading_zeros()) as usize) * 64
    } else if lo < DC_BASE {
        P_AC + (lo - AC_BASE) / 16 / NREM * 16
    } else {
        P_DC + (lo - DC_BASE) / 16 / DC_SUB * 16
    }
}
fn parent_groups() -> impl Iterator<Item = (usize, usize)> {
    (0..16).map(|c| (P_NZ + c * 64, 64)).chain((0..2 * 63 + 2 * 9).map(|c| (P_AC + c * 16, 16)))
}

// AC parents may in turn inherit from a grandparent: (component class, band of positions).
const N_BAND: usize = 10;
/// band of AC position k (1..63)
static KBAND: [u8; 64] = {
    let ends = [3, 6, 10, 15, 21, 28, 36, 45, 55, 64];
    let mut t = [0u8; 64];
    let mut k = 1;
    let mut b = 0;
    while k < 64 {
        while k >= ends[b] {
            b += 1;
        }
        t[k] = b as u8;
        k += 1;
    }
    t
};
const G_SYM: usize = 2 * N_BAND * 16;
/// Grandparent (start in the grandparent symbol space) of an AC parent starting at `plo`.
#[inline(always)]
fn grand_of(plo: usize) -> Option<usize> {
    (P_AC..P_DC).contains(&plo).then(|| {
        let p = (plo - P_AC) / 16;
        ((p / 63) * N_BAND + KBAND[p % 63 + 1] as usize) * 16
    })
}

/// How the tables are transmitted: per group either its own table at some precision, or the
/// table of its parent.
struct TablePlan {
    /// per group (in `groups()` order): Some(shift) = own table, None = inherit the parent
    own: Vec<Option<u8>>,
    /// per parent group (in `parent_groups()` order): Some(shift) if transmitted
    parent: Vec<Option<u8>>,
    /// normalised parent frequencies (PROB_SCALE scale), [P_SYM]
    pfreq: Vec<u16>,
    /// per parent group: inherits its grandparent (AC only)
    pinherit: Vec<bool>,
    /// per grandparent: Some(shift) if transmitted
    grand: Vec<Option<u8>>,
    /// normalised grandparent frequencies, [G_SYM]
    gfreq: Vec<u16>,
}

/// Normalised frequencies (sum PROB_SCALE per group; unused groups get a dummy distribution)
/// and the transmission plan: each table gets the precision that minimises table bits + coded
/// data bits (groups with few samples get coarse tables), and groups whose own table is not
/// worth its cost inherit the distribution of their parent.
fn normalize(hist: &[u32]) -> (Vec<u16>, TablePlan) {
    let mut tmp = [0u16; 64];
    let mut phist = vec![0u32; P_SYM];
    for (lo, len) in groups() {
        let plo = parent_of(lo);
        for a in 0..len {
            phist[plo + a] += hist[lo + a];
        }
    }
    // grandparents of the AC parents
    let mut ghist = vec![0u32; G_SYM];
    for (plo, len) in parent_groups() {
        if let Some(glo) = grand_of(plo) {
            for a in 0..len {
                ghist[glo + a] += phist[plo + a];
            }
        }
    }
    let mut gfreq = vec![0u16; G_SYM];
    let mut gshift = [0u8; G_SYM / 16];
    for g in 0..G_SYM / 16 {
        let h = &ghist[g * 16..g * 16 + 16];
        if h.iter().any(|&x| x > 0) {
            let sh = best_shift(h, &mut tmp).1;
            norm_group(h, PROB_SCALE >> sh, &mut tmp[..16]);
            for a in 0..16 {
                gfreq[g * 16 + a] = tmp[a] << sh;
            }
            gshift[g] = sh as u8;
        }
    }
    let mut pfreq = vec![0u16; P_SYM];
    let mut pshift = Vec::new();
    let mut pinherit = Vec::new();
    for (plo, len) in parent_groups() {
        let h = &phist[plo..plo + len];
        let mut sh = 0;
        let mut inh = false;
        if h.iter().any(|&x| x > 0) {
            let (cost, s) = best_shift(h, &mut tmp);
            sh = s;
            if let Some(glo) = grand_of(plo) {
                let gf = &gfreq[glo..glo + len];
                let inherit: f64 = (0..len)
                    .filter(|&a| h[a] > 0)
                    .map(|a| h[a] as f64 * (PROB_SCALE as f64 / gf[a] as f64).log2())
                    .sum();
                inh = inherit < cost;
            }
            if inh {
                pfreq[plo..plo + len].copy_from_slice(&gfreq[grand_of(plo).unwrap()..][..len]);
            } else {
                norm_group(h, PROB_SCALE >> sh, &mut tmp[..len]);
                for a in 0..len {
                    pfreq[plo + a] = tmp[a] << sh;
                }
            }
        }
        pshift.push(sh as u8);
        pinherit.push(inh);
    }
    let pindex = |plo: usize| parent_groups().position(|(x, _)| x == plo).unwrap();
    let mut freq = vec![0u16; N_SYM];
    let mut own = Vec::with_capacity(N_SYM / 16);
    let mut parent = vec![None; pshift.len()];
    for (lo, len) in groups() {
        let h = &hist[lo..lo + len];
        let f = &mut freq[lo..lo + len];
        if h.iter().all(|&x| x == 0) {
            f[0] = PROB_SCALE as u16;
            own.push(Some(0));
            continue;
        }
        let (cost, sh) = best_shift(h, &mut tmp);
        let plo = parent_of(lo);
        let pf = &pfreq[plo..plo + len];
        let inherit: f64 =
            (0..len).filter(|&a| h[a] > 0).map(|a| h[a] as f64 * (PROB_SCALE as f64 / pf[a] as f64).log2()).sum();
        if inherit < cost {
            f.copy_from_slice(pf);
            own.push(None);
            let pi = pindex(plo);
            parent[pi] = Some(pshift[pi]);
            continue;
        }
        norm_group(h, PROB_SCALE >> sh, &mut tmp[..len]);
        for a in 0..len {
            f[a] = tmp[a] << sh;
        }
        own.push(Some(sh as u8));
    }
    let mut grand = vec![None; G_SYM / 16];
    for (((plo, _), p), &inh) in parent_groups().zip(&parent).zip(&pinherit) {
        if let (Some(_), true) = (p, inh) {
            let g = grand_of(plo).unwrap() / 16;
            grand[g] = Some(gshift[g]);
        }
    }
    (freq, TablePlan { own, parent, pfreq, pinherit, grand, gfreq })
}

/// Encoder (`want_enc`) or decoder tables from normalised frequencies (every group sums to
/// PROB_SCALE).
fn build_tables(freq: Vec<u16>, want_enc: bool) -> Tables {
    let mut cum = vec![0u16; N_SYM];
    let mut enc = vec![EncSym::default(); if want_enc { N_SYM } else { 0 }];
    for (lo, len) in groups() {
        let mut s = 0u32;
        for a in lo..lo + len {
            cum[a] = s as u16;
            let fr = freq[a] as u32;
            if want_enc && fr > 0 {
                let e = &mut enc[a];
                e.cmpl_freq = (PROB_SCALE - fr) as u16;
                if fr < 2 {
                    e.rcp_freq = !0u64;
                    e.rcp_shift = 0;
                    e.bias = s + PROB_SCALE - 1;
                } else {
                    let mut shift = 0;
                    while fr > (1u32 << shift) {
                        shift += 1;
                    }
                    let x1 = 1u64 << (shift + 31);
                    let t1 = x1 / fr as u64;
                    let t0 = ((x1 % fr as u64) << 32).div_ceil(fr as u64);
                    e.rcp_freq = t0 + (t1 << 32);
                    e.rcp_shift = (shift - 1) as u16;
                    e.bias = s;
                }
            }
            s += fr;
        }
    }
    if want_enc {
        return Tables { enc, cf: Vec::new(), lut: Vec::new() };
    }
    let cf: Vec<u32> = (0..N_SYM).map(|a| cum[a] as u32 | (freq[a] as u32) << 16).collect();
    let mut lut = vec![0u8; N_SYM / 16 * 64];
    for (lo, len) in groups() {
        let mut sym = 0;
        for b in 0..64u32 {
            let start = b << (PROB_BITS - 6);
            while start >= cum[lo + sym] as u32 + freq[lo + sym] as u32 {
                sym += 1;
            }
            debug_assert!(sym < len);
            lut[(lo >> 4) * 64 + b as usize] = sym as u8;
        }
    }
    Tables { enc, cf, lut }
}

// ---------------------------------------------------------------- serialisation

/// LSB-first bit reader over a byte slice (for the table section only).
struct SmallBits<'a> {
    d: &'a [u8],
    bp: usize,
}
impl SmallBits<'_> {
    /// next 32 bits (zero past the end)
    #[inline(always)]
    fn peek(&self) -> u64 {
        let p = self.bp >> 3;
        let mut w = [0u8; 8];
        if let Some(s) = self.d.get(p..p + 8) {
            w.copy_from_slice(s);
        } else if p < self.d.len() {
            w[..self.d.len() - p].copy_from_slice(&self.d[p..]);
        }
        u64::from_le_bytes(w) >> (self.bp & 7)
    }
    fn bit(&mut self) -> Result<u32, Error> {
        if self.bp >= self.d.len() * 8 {
            return Err(Error("truncated tables".into()));
        }
        let v = (self.peek() & 1) as u32;
        self.bp += 1;
        Ok(v)
    }
    /// Elias gamma (value >= 1): n zeros, then the value's n + 1 bits MSB first.
    #[inline(always)]
    fn gamma(&mut self) -> Result<u32, Error> {
        let w = self.peek();
        let n = (w as u32).trailing_zeros();
        if n > 13 || self.bp + 2 * n as usize + 1 > self.d.len() * 8 {
            return Err(Error("bad gamma code".into()));
        }
        let bits = ((w >> n) as u32) & ((2 << n) - 1);
        self.bp += 2 * n as usize + 1;
        Ok(bits.reverse_bits() >> (31 - n))
    }
}
#[inline(always)]
fn put_gamma(w: &mut BitWriter, v: u32) {
    let n = 31 - v.leading_zeros(); // v >= 1, v <= 4097 -> n <= 12
    w.reserve(8);
    w.put(2 * n + 1, (v.reverse_bits() >> (31 - n)) << n);
}

fn put_table(w: &mut BitWriter, f: &[u16], sh: u8) {
    w.reserve(8);
    w.put(4, sh as u32);
    for &x in &f[..f.len() - 1] {
        put_gamma(w, (x >> sh) as u32 + 1);
    }
}
fn get_table(r: &mut SmallBits, f: &mut [u16]) -> Result<(), Error> {
    let mut sh = 0;
    for i in 0..4 {
        sh |= r.bit()? << i;
    }
    if sh > MAX_SHIFT {
        return Err(Error("bad table precision".into()));
    }
    let total = PROB_SCALE >> sh;
    let mut sum = 0u32;
    let n = f.len();
    for x in &mut f[..n - 1] {
        let v = r.gamma()? - 1;
        sum += v;
        if sum > total {
            return Err(Error("bad frequency table".into()));
        }
        *x = (v << sh) as u16;
    }
    f[n - 1] = ((total - sum) << sh) as u16;
    Ok(())
}

/// Used-group bitmap; per used group an "inherit the parent" bit; per inherited AC parent an
/// "inherit the grandparent" bit; the grandparent tables that are inherited; the parent tables
/// that are inherited and not themselves inheriting; then the own tables. A table is a 4-bit precision shift and the frequencies (at
/// that precision) of all but the last symbol as gamma(f + 1); the last one is implied.
fn write_tables(freq: &[u16], plan: &TablePlan, hist: &[u32]) -> Vec<u8> {
    let mut w = BitWriter::with_capacity(4096);
    let used: Vec<bool> = groups().map(|(lo, len)| hist[lo..lo + len].iter().any(|&x| x > 0)).collect();
    for &u in &used {
        w.reserve(8);
        w.put(1, u as u32);
    }
    for ((_, &u), own) in groups().zip(&used).zip(&plan.own) {
        if u {
            w.reserve(8);
            w.put(1, own.is_none() as u32);
        }
    }
    let dbg = std::env::var_os("JRC_DEBUG_TABLES").is_some();
    let mut mark = w.out.len();
    let mut report = |w: &BitWriter, what: &str, n: usize| {
        if dbg {
            eprintln!("  tables: {what:<12} {:6} B  ({n} tables)", w.out.len() - mark);
        }
        mark = w.out.len();
    };
    for (((plo, _), p), &inh) in parent_groups().zip(&plan.parent).zip(&plan.pinherit) {
        if p.is_some() && grand_of(plo).is_some() {
            w.reserve(8);
            w.put(1, inh as u32);
        }
    }
    report(&w, "bitmaps", 0);
    let ng = plan.grand.iter().filter(|g| g.is_some()).count();
    for (g, sh) in plan.grand.iter().enumerate() {
        if let Some(sh) = sh {
            put_table(&mut w, &plan.gfreq[g * 16..g * 16 + 16], *sh);
        }
    }
    report(&w, "grandparents", ng);
    let mut np = 0;
    for (((plo, len), sh), &inh) in parent_groups().zip(&plan.parent).zip(&plan.pinherit) {
        if let (Some(sh), false) = (sh, inh) {
            put_table(&mut w, &plan.pfreq[plo..plo + len], *sh);
            np += 1;
        }
    }
    report(&w, "parents", np);
    for (name, range) in [("nz", NZ_BASE..AC_BASE), ("ac", AC_BASE..DC_BASE), ("dc", DC_BASE..N_SYM)] {
        let mut n = 0;
        for (((lo, len), &u), own) in groups().zip(&used).zip(&plan.own) {
            if let (true, Some(sh), true) = (u, own, range.contains(&lo)) {
                put_table(&mut w, &freq[lo..lo + len], *sh);
                n += 1;
            }
        }
        report(&w, name, n);
    }
    w.finish()
}

fn read_tables(d: &[u8]) -> Result<Vec<u16>, Error> {
    let mut r = SmallBits { d, bp: 0 };
    let used: Vec<bool> = groups().map(|_| r.bit().map(|b| b != 0)).collect::<Result<_, _>>()?;
    let mut inherit = vec![false; used.len()];
    let mut need = vec![false; P_SYM];
    for (gi, ((lo, _), &u)) in groups().zip(&used).enumerate() {
        if u {
            inherit[gi] = r.bit()? != 0;
            need[parent_of(lo)] |= inherit[gi];
        }
    }
    let mut pinh = vec![false; P_SYM];
    let mut gneed = vec![false; G_SYM];
    for (plo, _) in parent_groups() {
        if let (true, Some(glo)) = (need[plo], grand_of(plo)) {
            pinh[plo] = r.bit()? != 0;
            gneed[glo] |= pinh[plo];
        }
    }
    let mut gfreq = vec![0u16; G_SYM];
    for g in 0..G_SYM / 16 {
        if gneed[g * 16] {
            get_table(&mut r, &mut gfreq[g * 16..g * 16 + 16])?;
        }
    }
    let mut pfreq = vec![0u16; P_SYM];
    for (plo, len) in parent_groups() {
        if pinh[plo] {
            let glo = grand_of(plo).unwrap();
            pfreq[plo..plo + len].copy_from_slice(&gfreq[glo..glo + len]);
        } else if need[plo] {
            get_table(&mut r, &mut pfreq[plo..plo + len])?;
        }
    }
    let mut freq = vec![0u16; N_SYM];
    for (gi, ((lo, len), &u)) in groups().zip(&used).enumerate() {
        if !u {
            freq[lo] = PROB_SCALE as u16;
        } else if inherit[gi] {
            let plo = parent_of(lo);
            freq[lo..lo + len].copy_from_slice(&pfreq[plo..plo + len]);
        } else {
            get_table(&mut r, &mut freq[lo..lo + len])?;
        }
    }
    Ok(freq)
}

// ---------------------------------------------------------------- rANS

/// Encode one symbol into state `x`, writing a renormalisation word below `out[*w]`.
/// SAFETY: the caller guarantees `*w` > 0 (at most one word per symbol is written).
#[inline(always)]
unsafe fn rans_put(x: &mut u64, e: &EncSym, out: *mut u32, w: &mut usize) {
    let xmax = ((RANS_L >> PROB_BITS) << 32) * (PROB_SCALE - e.cmpl_freq as u32) as u64;
    if *x >= xmax {
        *w -= 1;
        unsafe { *out.add(*w) = *x as u32 };
        *x >>= 32;
    }
    let q = ((*x as u128 * e.rcp_freq as u128) >> 64) as u64 >> e.rcp_shift;
    *x = *x + e.bias as u64 + q * e.cmpl_freq as u64;
}

/// `buf` (written from `w` on) with room for at least `need` more words before `w`: the
/// words moved to the end of a larger buffer if necessary.
#[cold]
#[inline(never)]
fn rans_room(buf: &mut Vec<u32>, w: &mut usize, need: usize) {
    if *w >= need {
        return;
    }
    let n = buf.len() - *w;
    let mut b = vec![0; (buf.len() * 2).max(n + need)];
    let w2 = b.len() - n;
    b[w2..].copy_from_slice(&buf[*w..]);
    (*buf, *w) = (b, w2);
}

/// Symbols are encoded last to first; the words come out in decoding order (the four final
/// states first). The buffer starts at a typical size and grows as needed: every 1024
/// symbols, room for as many words is made. Kept out of line: inlined into a caller, the four
/// states get spilled.
#[inline(never)]
fn rans_encode(syms: &[u16], t: &Tables) -> Vec<u32> {
    const STEP: usize = 1024;
    let n = syms.len();
    let mut buf = vec![0u32; n / 8 + STEP + 8];
    let mut w = buf.len();
    let (mut x0, mut x1, mut x2, mut x3) = (RANS_L, RANS_L, RANS_L, RANS_L);
    let full = n & !3;
    let enc = |s: u16| -> &EncSym { &t.enc[s as usize] };
    rans_room(&mut buf, &mut w, (n - full) + 8);
    let mut p = buf.as_mut_ptr();
    // SAFETY: there is room for a word per symbol (and the final states) up to the next check
    unsafe {
        for j in (full..n).rev() {
            let e = enc(syms[j]);
            match j & 3 {
                0 => rans_put(&mut x0, e, p, &mut w),
                1 => rans_put(&mut x1, e, p, &mut w),
                _ => rans_put(&mut x2, e, p, &mut w),
            }
        }
        for c in syms[..full].rchunks(STEP) {
            if w < c.len() + 8 {
                rans_room(&mut buf, &mut w, c.len() + 8);
                p = buf.as_mut_ptr();
            }
            for g in c.chunks_exact(4).rev() {
                rans_put(&mut x3, enc(g[3]), p, &mut w);
                rans_put(&mut x2, enc(g[2]), p, &mut w);
                rans_put(&mut x1, enc(g[1]), p, &mut w);
                rans_put(&mut x0, enc(g[0]), p, &mut w);
            }
        }
        for x in [x3, x2, x1, x0] {
            w -= 2;
            *p.add(w) = (x >> 32) as u32;
            *p.add(w + 1) = x as u32;
        }
    }
    buf.drain(..w);
    buf.shrink_to_fit();
    buf
}

const RAW_PAD: usize = 256;
const SRC_PAD: usize = 80;

struct Dec<'a> {
    x: [u64; 4],
    src: &'a [u32],
    pos: usize,
    t: &'a Tables,
    raw: &'a [u8],
    bp: usize,       // raw bit position
    raw_bits: usize, // length of the real raw stream in bits
    src_len: usize,  // number of real rANS words
}
impl<'a> Dec<'a> {
    /// Decode one symbol of the group starting at compact index `base`.
    #[inline(always)]
    fn sym(&mut self, base: usize) -> u32 {
        let x = self.x[0];
        let slot = (x & (PROB_SCALE as u64 - 1)) as u32;
        // SAFETY: base is a group start (multiple of 16) < N_SYM, slot < 4096, and the
        // refinement stops inside the group because the group's frequencies sum to 4096.
        let (s, cf) = unsafe {
            let mut s = *self.t.lut.get_unchecked((base >> 4) * 64 + (slot >> (PROB_BITS - 6)) as usize) as usize;
            let mut cf = *self.t.cf.get_unchecked(base + s);
            while slot >= (cf & 0xffff) + (cf >> 16) {
                s += 1;
                cf = *self.t.cf.get_unchecked(base + s);
            }
            (s, cf)
        };
        let (c, f) = ((cf & 0xffff) as u64, (cf >> 16) as u64);
        let x = f * (x >> PROB_BITS) + slot as u64 - c;
        // branchless renormalisation; SAFETY: `src` carries SRC_PAD words of padding and
        // `walk_dec` checks once per block (<= 65 symbols) that pos stays within the real words
        let next = unsafe { *self.src.get_unchecked(self.pos) } as u64;
        let low = x < RANS_L;
        // renormalisation happens every few symbols in an irregular pattern: keep it a select
        let x = std::hint::select_unpredictable(low, (x << 32) | next, x);
        self.pos += low as usize;
        // rotate: symbol j uses state j % 4
        self.x = [self.x[1], self.x[2], self.x[3], x];
        s as u32
    }
    /// `n` raw bits (n <= 16), LSB first
    #[inline(always)]
    fn raw(&mut self, n: u32) -> u32 {
        let start = self.bp;
        // SAFETY: `walk_dec` checks once per block that bp stays within the real stream, and the
        // buffer has RAW_PAD bytes of padding, more than one block can consume (64 * 16 bits).
        let w = unsafe { (self.raw.as_ptr().add(start >> 3) as *const u64).read_unaligned() }.to_le();
        self.bp += n as usize;
        ((w >> (start & 7)) & ((1u64 << n) - 1)) as u32
    }
}

#[inline(never)]
fn walk_dec(
    d: &mut Dec,
    blocks: &mut [[i16; 64]],
    masks: &mut [u64],
    bw: usize,
    bh: usize,
    cc: usize,
    dcb: &DcBound,
) -> Result<(), Error> {
    let mut nzs = vec![0u8; bw * bh];
    let mut dcc = vec![0u8; bw * bh];
    let mut bottom = vec![0i32; bw];
    let mut right = 0i32;
    let ac_idx = &AC_IDX[cc];
    for by in 0..bh {
        for bx in 0..bw {
            let i = by * bw + bx;
            let (has_a, has_l) = (by > 0, bx > 0);
            let (nza, nzl) = (if has_a { nzs[i - bw] } else { 0 }, if has_l { nzs[i - 1] } else { 0 });
            let nz = d.sym(nz_ctx(cc, nza as u32, nzl as u32, has_a, has_l));
            nzs[i] = nz as u8;
            let b = &mut blocks[i];
            *b = [0; 64];
            let mut rem = nz as usize;
            let mut k = 1;
            let mut base = ac_idx[1][rem] as usize;
            let mut mask = 0u64;
            while rem > 0 && k < 64 {
                // both candidate contexts of the next position, computed off the symbol chain
                let row = &ac_idx[(k + 1) & 63];
                let (keep, dec) = (row[rem] as usize, row[rem - 1] as usize);
                let cat = d.sym(base);
                let w = d.raw(cat);
                b[k] = join_value(cat, w) as i16;
                let nzv = cat != 0;
                mask |= (nzv as u64) << k;
                rem -= nzv as usize;
                base = if nzv { dec } else { keep };
                k += 1;
            }
            let (pred, sub) = if DC_BOUNDARY {
                dcb.pred(&blocks[i], bottom[bx], right, has_a, has_l)
            } else {
                (dc_pred(blocks, i, bw, bx, by), NZ_B[nz as usize] as usize)
            };
            let cs = if has_a { dcc[i - bw] as u32 } else { 0 } + if has_l { dcc[i - 1] as u32 } else { 0 };
            let cat = d.sym(dc_ctx(cc, cs, sub));
            let w = d.raw(cat);
            let dc = pred + join_value(cat, w);
            blocks[i][0] = dc as i16;
            if DC_BOUNDARY {
                (bottom[bx], right) = dcb.bounds(&blocks[i]);
            }
            masks[i] = mask | (dc != 0) as u64;
            dcc[i] = cat as u8;
            if d.bp > d.raw_bits || d.pos > d.src_len {
                return Err(Error("corrupt stream".into()));
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------- stripes

/// The image is coded as independent horizontal stripes (whole MCU rows; every component's
/// rows of those MCUs), each with its own rANS and raw streams, so that they can be modelled
/// and decoded in parallel. Contexts do not cross stripe boundaries. The tables are shared.
/// Target stripe size in blocks: large enough that the lost above-context of each stripe's
/// first row costs little, small enough to balance across cores.
const STRIPE_BLOCKS: usize = 1 << 15;
pub const MAX_STRIPES: usize = 256;

/// Number of MCU rows (of the image's block grid; single-component images: block rows).
fn mcu_rows(j: &Jpeg) -> usize {
    if j.comps.len() == 1 { j.comps[0].bh } else { j.comps[0].bh / j.comps[0].v }
}

/// Block rows of component `ci` in stripe `s` of `n`.
fn stripe_rows(j: &Jpeg, ci: usize, s: usize, n: usize) -> std::ops::Range<usize> {
    let rows = mcu_rows(j);
    let f = if j.comps.len() == 1 { 1 } else { j.comps[ci].v };
    s * rows / n * f..(s + 1) * rows / n * f
}

pub(crate) fn stripe_count(j: &Jpeg) -> usize {
    let nblk: usize = j.comps.iter().map(|c| c.bw * c.bh).sum();
    (nblk / STRIPE_BLOCKS).clamp(1, mcu_rows(j).clamp(1, MAX_STRIPES))
}

/// A stored stripe count must leave every stripe at least one MCU row.
pub(crate) fn check_stripes(j: &Jpeg, ns: usize) -> Result<(), Error> {
    if ns == 0 || ns > MAX_STRIPES || ns > mcu_rows(j).max(1) {
        return Err(Error("bad stripe count".into()));
    }
    Ok(())
}

/// The MCUs of each of `ns` stripes, for a scan that codes every component in MCU rows of
/// `mcux` MCUs (the stripes' rows are whole MCU rows of it).
pub(crate) fn stripe_mcus(j: &Jpeg, mcux: usize, ns: usize) -> Vec<(usize, usize)> {
    let rows = mcu_rows(j);
    (0..ns).map(|s| (s * rows / ns * mcux, (s + 1) * rows / ns * mcux)).collect()
}

/// `bufs` holding the rows of stripe `si` of `ns` (contents unspecified).
pub(crate) fn hold_stripe(j: &Jpeg, si: usize, ns: usize, bufs: &mut Vec<Coefs>) {
    bufs.resize_with(j.comps.len(), Coefs::default);
    bufs.truncate(j.comps.len());
    for (ci, (b, c)) in bufs.iter_mut().zip(&j.comps).enumerate() {
        b.hold(c.bw, stripe_rows(j, ci, si, ns));
    }
}

// ---------------------------------------------------------------- public API

pub struct Stripe {
    pub rans: Vec<u32>,
    pub raw: Vec<u8>,
}

pub struct Encoded {
    pub tables: Vec<u8>,
    pub stripes: Vec<Stripe>,
    pub nsym: usize,
}

pub(crate) fn get_varint(d: &[u8], p: &mut usize) -> Result<u64, Error> {
    varint::get(d, p).map_err(|e| Error(e.0.into()))
}
pub(crate) fn get_bytes<'a>(d: &'a [u8], p: &mut usize, n: u64) -> Result<&'a [u8], Error> {
    let n = usize::try_from(n).map_err(|_| Error("bad length".into()))?;
    let s = d.get(*p..p.checked_add(n).ok_or(Error("bad length".into()))?).ok_or(Error("truncated".into()))?;
    *p += n;
    Ok(s)
}

impl Stripe {
    /// The rANS words as stored: little-endian bytes.
    pub fn rans_bytes(&self) -> std::borrow::Cow<'_, [u8]> {
        if cfg!(target_endian = "little") {
            // SAFETY: u32 has no padding; on little-endian targets its bytes are the LE encoding
            std::borrow::Cow::Borrowed(unsafe {
                std::slice::from_raw_parts(self.rans.as_ptr() as *const u8, self.rans.len() * 4)
            })
        } else {
            std::borrow::Cow::Owned(self.rans.iter().flat_map(|w| w.to_le_bytes()).collect())
        }
    }
}

impl Encoded {
    pub fn size(&self) -> usize {
        self.tables.len() + self.stripes.iter().map(|s| s.rans.len() * 4 + s.raw.len()).sum::<usize>()
    }
}

pub use crate::model::Phases;

/// A modelled stripe: its symbols (as in `Sink`, coded once the tables are known) and raw
/// bits.
struct Walked {
    idx: Vec<u16>,
    last: Vec<u8>,
    /// followed by 63 bytes of padding
    ac: Vec<u8>,
    /// per component: number of blocks, component class
    comps: Vec<(usize, usize)>,
    nsym: usize,
    raw: Vec<u8>,
}

impl Walked {
    /// The symbols' compact indices, in coding order, into `out`.
    fn expand(&self, out: &mut Vec<u16>) {
        // every block's AC symbols are written as a whole row of 63 (vectorised), the excess
        // overwritten by what follows
        out.clear();
        out.resize(self.nsym + 64, 0);
        let (mut n, mut nc) = (0, 0);
        let mut b = 0;
        for &(nb, cc) in &self.comps {
            let pos: &[u16; 63] = AC_POS[cc][1..].try_into().unwrap();
            for (&last, idx) in self.last[b..b + nb].iter().zip(self.idx[2 * b..2 * (b + nb)].chunks_exact(2)) {
                let last = last as usize;
                out[n] = idx[0];
                let dst: &mut [u16; 63] = (&mut out[n + 1..n + 64]).try_into().unwrap();
                let src: &[u8; 63] = self.ac[nc..nc + 63].try_into().unwrap();
                for i in 0..63 {
                    dst[i] = pos[i] + src[i] as u16;
                }
                out[n + 1 + last] = idx[1];
                (n, nc) = (n + 2 + last, nc + last);
            }
            b += nb;
        }
        debug_assert_eq!(n, self.nsym);
        out.truncate(n);
    }
}

thread_local! {
    static SYMS: std::cell::RefCell<Vec<u16>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Models a file stripe by stripe, in any order and on any threads, then codes it: the
/// coefficients need only be held a stripe at a time.
pub(crate) struct Modeller {
    ns: usize,
    dcb: Vec<DcBound>,
    hist: std::sync::Mutex<Vec<u32>>,
    walked: std::sync::Mutex<Vec<Option<Walked>>>,
}

impl Modeller {
    pub(crate) fn new(j: &Jpeg, ns: usize) -> Modeller {
        Modeller {
            ns,
            dcb: j.comps.iter().map(|c| DcBound::new(&j.qt[c.tq])).collect(),
            hist: std::sync::Mutex::new(vec![0; N_SYM]),
            walked: std::sync::Mutex::new((0..ns).map(|_| None).collect()),
        }
    }

    /// Model stripe `si` from `coefs` (zigzag order, with nonzero masks), which hold at least
    /// its rows.
    pub(crate) fn stripe(&self, j: &Jpeg, si: usize, coefs: &[Coefs]) {
        let nblk: usize = (0..j.comps.len()).map(|ci| stripe_rows(j, ci, si, self.ns).len() * coefs[ci].bw).sum();
        let mut s = Sink {
            idx: vec![0; nblk * 2],
            ni: 0,
            last: vec![0; nblk],
            ac: vec![0; nblk * 16 + 63],
            nc: 0,
            hist: vec![0; N_SYM],
            raw: BitWriter::with_capacity(nblk * 8),
        };
        let mut comps = Vec::with_capacity(coefs.len());
        for (ci, c) in coefs.iter().enumerate() {
            let rows = stripe_rows(j, ci, si, self.ns);
            let (a, b) = ((rows.start - c.y0) * c.bw, (rows.end - c.y0) * c.bw);
            walk_enc(&mut s, &c.blocks[a..b], &c.masks[a..b], c.bw, ci.min(1), &self.dcb[ci]);
            comps.push((b - a, ci.min(1)));
        }
        let Sink { mut idx, ni, mut last, mut ac, nc, hist, raw } = s;
        idx.truncate(ni);
        idx.shrink_to_fit();
        last.truncate(ni / 2);
        last.shrink_to_fit();
        ac.truncate(nc);
        ac.resize(nc + 63, 0);
        ac.shrink_to_fit();
        for (h, x) in self.hist.lock().unwrap().iter_mut().zip(&hist) {
            *h += x;
        }
        self.walked.lock().unwrap()[si] = Some(Walked { idx, last, ac, comps, nsym: ni + nc, raw: raw.finish() });
    }

    /// The tables and the stripes' rANS words, once every stripe is modelled; with the seconds
    /// spent on each.
    pub(crate) fn finish(self) -> (Encoded, f64, f64) {
        use crate::par::prelude::*;
        let t0 = Instant::now();
        let hist = self.hist.into_inner().unwrap();
        let (freq, plan) = normalize(&hist);
        let tables = write_tables(&freq, &plan, &hist);
        let t = build_tables(freq, true);
        let t1 = Instant::now();
        let walked: Vec<Walked> =
            self.walked.into_inner().unwrap().into_iter().map(|w| w.expect("stripe not modelled")).collect();
        let nsym = walked.iter().map(|w| w.nsym).sum();
        // each stripe's symbols are dropped as soon as they are coded
        let stripes = walked
            .into_par_iter()
            .map(|w| {
                let mut syms = SYMS.with(|b| std::mem::take(&mut *b.borrow_mut()));
                w.expand(&mut syms);
                let rans = rans_encode(&syms, &t);
                SYMS.with(|b| *b.borrow_mut() = syms);
                Stripe { rans, raw: w.raw }
            })
            .collect();
        let norm = (t1 - t0).as_secs_f64();
        (Encoded { tables, stripes, nsym }, norm, t1.elapsed().as_secs_f64())
    }
}

/// `coefs` must be in zigzag order (`jpeg::Order::Zigzag`), with nonzero masks.
pub fn encode(j: &Jpeg, coefs: &[Coefs]) -> (Encoded, Phases) {
    use crate::par::prelude::*;
    let ns = stripe_count(j);
    let m = Modeller::new(j, ns);
    let t0 = Instant::now();
    (0..ns).into_par_iter().for_each(|si| m.stripe(j, si, coefs));
    let walk = t0.elapsed().as_secs_f64();
    let (e, norm, rans) = m.finish();
    (e, Phases { walk, norm, rans })
}

/// Decoding tables from a stored table section.
pub(crate) fn read_tables_section(tables: &[u8]) -> Result<Tables, Error> {
    Ok(build_tables(read_tables(tables)?, false))
}

/// Decode one stripe from its rANS words (LE bytes) and raw bytes into the rows `parts` (per
/// component, in order).
fn decode_rows<'a>(
    j: &Jpeg,
    t: &Tables,
    (words, raw): (&[u8], &[u8]),
    parts: impl Iterator<Item = (&'a mut [[i16; 64]], &'a mut [u64])>,
) -> Result<(), Error> {
    if words.len() < 32 || !words.len().is_multiple_of(4) {
        return Err(Error("bad rANS stream length".into()));
    }
    // Reads of a corrupt stream may run up to one block past the end before `walk_dec`
    // notices: pad the rANS words (<= 1 per symbol, <= 65 symbols per block) and raw bits.
    let nw = words.len() / 4;
    let mut src = Vec::with_capacity(nw + SRC_PAD);
    src.extend(words.chunks_exact(4).map(|c| u32::from_le_bytes(c.try_into().unwrap())));
    src.resize(nw + SRC_PAD, 0);
    let mut rb = Vec::with_capacity(raw.len() + RAW_PAD);
    rb.extend_from_slice(raw);
    rb.resize(raw.len() + RAW_PAD, 0);
    let mut d = Dec { x: [0; 4], src: &src, pos: 8, t, raw: &rb, bp: 0, raw_bits: raw.len() * 8, src_len: nw };
    for k in 0..4 {
        d.x[k] = (src[2 * k] as u64) << 32 | src[2 * k + 1] as u64;
    }
    for (ci, (blocks, masks)) in parts.enumerate() {
        let c = &j.comps[ci];
        let dcb = DcBound::new(&j.qt[c.tq]);
        walk_dec(&mut d, blocks, masks, c.bw, blocks.len() / c.bw.max(1), ci.min(1), &dcb)?;
    }
    Ok(())
}

/// Decode stripe `si` of `ns` (its stored words and raw bytes) into `bufs`, which then hold its
/// rows (zigzag order, with nonzero masks).
pub(crate) fn decode_stripe(
    j: &Jpeg,
    t: &Tables,
    si: usize,
    ns: usize,
    stripe: (&[u8], &[u8]),
    bufs: &mut Vec<Coefs>,
) -> Result<(), Error> {
    check_stripes(j, ns)?;
    hold_stripe(j, si, ns, bufs);
    decode_rows(j, t, stripe, bufs.iter_mut().map(|c| (&mut c.blocks[..], &mut c.masks[..])))
}

/// Decode all stripes (stored words and raw bytes) into whole components `out` (zigzag order,
/// with nonzero masks).
pub(crate) fn decode_whole(
    j: &Jpeg,
    t: &Tables,
    stripes: &[(&[u8], &[u8])],
    out: &mut Vec<Coefs>,
) -> Result<(), Error> {
    use crate::par::prelude::*;
    let ns = stripes.len();
    check_stripes(j, ns)?;
    out.resize_with(j.comps.len(), Coefs::default);
    out.truncate(j.comps.len());
    // per stripe, per component: that stripe's rows of blocks and masks
    type Rows<'a> = (&'a mut [[i16; 64]], &'a mut [u64]);
    let mut parts: Vec<Vec<Rows>> = (0..ns).map(|_| Vec::new()).collect();
    for (ci, (c, o)) in j.comps.iter().zip(out.iter_mut()).enumerate() {
        o.hold(c.bw, 0..c.bh);
        let (mut blocks, mut masks) = (&mut o.blocks[..], &mut o.masks[..]);
        for (si, p) in parts.iter_mut().enumerate() {
            let n = stripe_rows(j, ci, si, ns).len() * c.bw;
            let (b, rb) = std::mem::take(&mut blocks).split_at_mut(n);
            let (m, rm) = std::mem::take(&mut masks).split_at_mut(n);
            (blocks, masks) = (rb, rm);
            p.push((b, m));
        }
    }
    parts.into_par_iter().zip(stripes).try_for_each(|(p, &s)| decode_rows(j, t, s, p.into_iter()))
}

/// Decodes into zigzag-ordered blocks, with nonzero masks.
pub fn decode(j: &Jpeg, e: &Encoded) -> Result<Vec<Coefs>, Error> {
    let mut out = Vec::new();
    decode_into(j, e, &mut out)?;
    Ok(out)
}

/// Like `decode`, reusing the allocations in `out`. The decoding tables are read from the
/// stored table section (`Encoded` holds the encoder's).
pub fn decode_into(j: &Jpeg, e: &Encoded, out: &mut Vec<Coefs>) -> Result<(), Error> {
    let t = read_tables_section(&e.tables)?;
    let words: Vec<_> = e.stripes.iter().map(|s| s.rans_bytes()).collect();
    let stripes: Vec<(&[u8], &[u8])> = words.iter().zip(&e.stripes).map(|(w, s)| (&w[..], &s.raw[..])).collect();
    decode_whole(j, &t, &stripes, out)
}

#[cfg(test)]
mod tests {
    use super::CV;

    #[test]
    fn cv_table_is_the_dct_basis() {
        for (v, &c) in CV.iter().enumerate() {
            let x = (v as f64 * std::f64::consts::PI / 16.0).cos()
                * if v == 0 { std::f64::consts::FRAC_1_SQRT_2 } else { 1.0 };
            // within an ulp of this platform's libm (the table holds the correctly rounded values)
            assert!(c.to_bits().abs_diff(x.to_bits()) <= 1, "c({v}) = {c} vs {x}");
        }
    }
}
