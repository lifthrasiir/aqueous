//! Entropy stage: context model over quantised DCT coefficients + static-table rANS.
//!
//! Model: 7x7 interior / edge / DC split with coefficient-domain 1-D boundary prediction
//! (no IDCT, fixed-point), static per-file histograms (two pass), 4-way interleaved 64-bit rANS
//! (reciprocal encoder, 32-bit IO). Blocks are in natural order (as produced by `jpeg::decode`),
//! nonzero masks in zigzag bit order.

use crate::jpeg::{Coefs, Jpeg};
use std::time::Instant;

const PROB_BITS: u32 = 12;
const PROB_SCALE: u32 = 1 << PROB_BITS;
const RANS_L: u64 = 1 << 31;
const NS: usize = 4; // interleaved rANS states

/// zigzag index -> natural index
const ZZ: [usize; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27,
    20, 13, 6, 7, 14, 21, 28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58,
    59, 52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
];

// ---------------------------------------------------------------- contexts
const N_NZ7: usize = 2 * 50;
const N_TOK7: usize = 2 * 49 * 10 * 6;
const N_NZE: usize = 2 * 15 * 8;
const N_TOKE: usize = 2 * 14 * 12 * 4;
const N_SGNE: usize = 2 * 14 * 4 * 6;
const N_DC: usize = 2 * 9 * 8;
const O_NZ7: usize = 0;
const O_TOK7: usize = O_NZ7 + N_NZ7;
const O_NZE: usize = O_TOK7 + N_TOK7;
const O_TOKE: usize = O_NZE + N_NZE;
const O_SGNE: usize = O_TOKE + N_TOKE;
const O_DC: usize = O_SGNE + N_SGNE;
const N_CTX: usize = O_DC + N_DC;

/// Compact symbol index: CTX_BASE[ctx] + symbol (each context owns `alphabet(ctx)` slots).
static CTX_BASE: [u32; N_CTX + 1] = {
    let mut t = [0u32; N_CTX + 1];
    let mut c = 0;
    while c < N_CTX {
        t[c + 1] = t[c] + alphabet(c) as u32;
        c += 1;
    }
    t
};
const N_SYM: usize = CTX_BASE[N_CTX] as usize;
const _: () = assert!(CTX_BASE[O_NZE] - CTX_BASE[O_TOK7] == (N_TOK7 * 16) as u32);

const fn alphabet(ctx: usize) -> usize {
    if ctx < O_TOK7 { 50 } else if ctx < O_NZE { 16 } else if ctx < O_TOKE { 15 } else if ctx < O_SGNE { 16 } else if ctx < O_DC { 2 } else { 16 }
}

fn bucket_u(x: u32, edges: &[u32]) -> u8 {
    edges.iter().filter(|&&e| e <= x).count() as u8
}
fn lut<const N: usize>(edges: &[u32]) -> [u8; N] {
    std::array::from_fn(|x| bucket_u(x as u32, edges))
}
#[inline(always)]
fn bitlen(x: i32) -> u32 {
    32 - x.unsigned_abs().leading_zeros()
}

const INTERIOR_MASK: u64 = {
    let mut m = 0u64;
    let mut z = 0;
    while z < 64 {
        if ZZ[z] / 8 >= 1 && ZZ[z] % 8 >= 1 {
            m |= 1 << z;
        }
        z += 1;
    }
    m
};
const EDGE_MASK: u64 = !INTERIOR_MASK & !1;

/// natural index of edge coefficient `ke`: row 0 (u = 1..7) then column 0 (v = 1..7)
#[inline(always)]
fn edge_pos(ke: usize) -> usize {
    if ke < 7 { ke + 1 } else { (ke - 6) * 8 }
}

// ---------------------------------------------------------------- coding interface
trait Io {
    fn sym(&mut self, ctx: usize, v: u32) -> u32;
    fn raw(&mut self, n: u32, v: u32) -> u32;
}

struct Component<'a> {
    bw: usize,
    bh: usize,
    w: Weights,
    blocks: &'a mut [[i16; 64]],
    /// nonzero masks (zigzag bit order) when encoding; decoding passes an empty slice
    masks: &'a [u64],
}

/// Lookup tables for context quantisation. Prediction thresholds are multiples of 0.5, so
/// bucketing floor(2|p|) through an integer LUT is exact.
struct Tables {
    interior: [usize; 49], // natural indices, in zigzag order
    e_lut: [u8; 256],      // 2*(|A|+|L|) -> 0..9
    e6_lut: [u16; 256],    // e_lut * 6 * 16 (compact-index stride of the TOK7 family)
    dens16_lut: [u16; 50 * 49], // dens_lut * 16
    dens_lut: Vec<u8>,     // [rem 0..49][k 0..48] -> 0..5
    n7_lut: [u8; 64],      // nz7 -> 0..7
    rem_e_lut: [u8; 16],   // remaining edge nz -> 0..3
    pe_lut: [u8; 256],     // floor(2|p|) -> 0..10
    ps_lut: [u8; 256],     // floor(2|p|) -> 0..5
    unc_lut: [u8; 128],    // floor(unc) -> 0..7
    dcn_lut: [u8; 64],     // dc cat sum -> 0..8
}

impl Tables {
    fn new() -> Self {
        let mut interior = [0; 49];
        let mut n = 0;
        for &nat in ZZ.iter() {
            if nat / 8 >= 1 && nat % 8 >= 1 {
                interior[n] = nat;
                n += 1;
            }
        }
        let mut dens_lut = vec![0u8; 50 * 49];
        for rem in 0..50u32 {
            for k in 0..49u32 {
                dens_lut[(rem * 49 + k) as usize] = bucket_u(rem * 64 / (49 - k), &[4, 8, 16, 32, 48]);
            }
        }
        let e_lut: [u8; 256] = lut(&[0, 1, 2, 3, 4, 6, 9, 14, 24]);
        Tables {
            interior,
            e_lut,
            e6_lut: std::array::from_fn(|i| e_lut[i] as u16 * 6 * 16),
            dens16_lut: std::array::from_fn(|i| dens_lut[i] as u16 * 16),
            dens_lut,
            n7_lut: lut(&[0, 1, 3, 6, 10, 16, 25]),
            rem_e_lut: lut(&[1, 2, 4]),
            pe_lut: lut(&[1, 2, 3, 5, 8, 12, 20, 32, 64, 128]),
            ps_lut: lut(&[1, 2, 4, 8, 16]),
            unc_lut: lut(&[1, 2, 4, 8, 16, 32, 64]),
            dcn_lut: lut(&[0, 1, 2, 4, 6, 8, 10, 12]),
        }
    }
}

// ---------------------------------------------------------------- boundary prediction
//
// Lakhani-style continuity across block boundaries, evaluated in the coefficient domain with
// 1-D DCT basis values at the boundary rows/columns. Fixed point (wrapping i32), so the result
// is exact, platform independent and independent of summation order -- the decoder may compute
// the sums in stages while the encoder does them in one go.

const WSHIFT: i32 = 10;

#[derive(Clone, Copy)]
struct Weights {
    wr: [i32; 64],  // [v][u]: cv[v] q, v >= 1      (row-0 / DC-from-above, column sums)
    wc: [i32; 64],  // [v][u]: cv[u] q, u >= 1      (col-0 / DC-from-left, row sums)
    wb: [i32; 64],  // [v][u]: cv[v] (-1)^v q        (bottom boundary of a block, column sums)
    wrt: [i32; 64], // [v][u]: cv[u] (-1)^u q        (right boundary of a block, row sums)
    inv_r2: [f32; 8], // 2 / (cv[0] q[0][u] 2^S): row-0 prediction in half quant steps
    inv_c2: [f32; 8], // 2 / (cv[0] q[v][0] 2^S)
    inv_dc: f32,      // 1 / (cv[0] q[0][0] 2^S)
}

impl Weights {
    fn new(q: &[f32; 64]) -> Self {
        let cv = |v: usize| (v as f64 * std::f64::consts::PI / 16.0).cos() * if v == 0 { std::f64::consts::FRAC_1_SQRT_2 } else { 1.0 };
        let sg = |v: usize| if v % 2 == 0 { 1.0 } else { -1.0 };
        let s = (1i32 << WSHIFT) as f64;
        let r = |x: f64| x.round() as i32;
        let mut w = Weights { wr: [0; 64], wc: [0; 64], wb: [0; 64], wrt: [0; 64], inv_r2: [0.0; 8], inv_c2: [0.0; 8], inv_dc: 0.0 };
        for v in 0..8 {
            for u in 0..8 {
                let qq = q[v * 8 + u] as f64;
                if v >= 1 {
                    w.wr[v * 8 + u] = r(cv(v) * qq * s);
                }
                if u >= 1 {
                    w.wc[v * 8 + u] = r(cv(u) * qq * s);
                }
                w.wb[v * 8 + u] = r(cv(v) * sg(v) * qq * s);
                w.wrt[v * 8 + u] = r(cv(u) * sg(u) * qq * s);
            }
            w.inv_r2[v] = (2.0 / (cv(0) * q[v] as f64 * s)) as f32;
            w.inv_c2[v] = (2.0 / (cv(0) * q[v * 8] as f64 * s)) as f32;
        }
        w.inv_dc = (1.0 / (cv(0) * q[0] as f64 * s)) as f32;
        w
    }
}

#[inline(always)]
fn widen(b: &[i16; 64]) -> [i32; 64] {
    std::array::from_fn(|k| b[k] as i32)
}
/// out[u] = sum_v w[v][u] x[v][u]
#[inline(always)]
fn col_sums(w: &[i32; 64], x: &[i32; 64]) -> [i32; 8] {
    let mut o = [0i32; 8];
    for v in 0..8 {
        for u in 0..8 {
            o[u] = o[u].wrapping_add(w[v * 8 + u].wrapping_mul(x[v * 8 + u]));
        }
    }
    o
}
/// out[v] = sum_u w[v][u] x[v][u]
#[inline(always)]
fn row_sums(w: &[i32; 64], x: &[i32; 64]) -> [i32; 8] {
    let mut o = [0i32; 8];
    for v in 0..8 {
        let mut acc = 0i32;
        for u in 0..8 {
            acc = acc.wrapping_add(w[v * 8 + u].wrapping_mul(x[v * 8 + u]));
        }
        o[v] = acc;
    }
    o
}

/// Edge predictions (in half quantisation steps): [0..7) row 0 (u = 1..7), [7..14) column 0.
/// Only needs the 7x7 interior of the current block (`sr`, `sc` entries 1..8).
#[inline(always)]
fn edge_preds(w: &Weights, bot_above: &[i32; 8], rgt_left: &[i32; 8], sr: &[i32; 8], sc: &[i32; 8]) -> [f32; 14] {
    let mut pe = [0f32; 14];
    for k in 1..8 {
        pe[k - 1] = bot_above[k].wrapping_sub(sr[k]) as f32 * w.inv_r2[k];
        pe[7 + k - 1] = rgt_left[k].wrapping_sub(sc[k]) as f32 * w.inv_c2[k];
    }
    pe
}

/// DC prediction and its uncertainty. Needs the edges of the current block (`sr[0]`, `sc[0]`).
#[inline(always)]
fn dc_pred(w: &Weights, bot_above: &[i32; 8], rgt_left: &[i32; 8], sr0: i32, sc0: i32, has_a: bool, has_l: bool) -> (i32, f32) {
    let dca = bot_above[0].wrapping_sub(sr0) as f32 * w.inv_dc;
    let dcl = rgt_left[0].wrapping_sub(sc0) as f32 * w.inv_dc;
    let (pf, unc) = match (has_a, has_l) {
        (true, true) => ((dca + dcl) * 0.5, (dca - dcl).abs()),
        (true, false) => (dca, 0.0),
        (false, true) => (dcl, 0.0),
        _ => (0.0, 0.0),
    };
    (pf.round() as i32, unc)
}

#[inline(always)]
fn half_idx(p2: f32) -> usize {
    (p2.abs() as u32).min(255) as usize // saturating float->int cast
}
/// sign class of a prediction given in half steps (p2 = 2p): 0 (p <= -0.25), 1, 2 (p >= 0.25)
#[inline(always)]
fn sign_class(p2: f32) -> usize {
    if p2 >= 0.5 { 2 } else if p2 <= -0.5 { 0 } else { 1 }
}

#[inline(always)]
fn code_value<I: Io>(io: &mut I, ctx: usize, x: i32) -> i32 {
    let cat = io.sym(ctx, bitlen(x));
    if cat == 0 {
        return 0;
    }
    // sign + (cat-1) extra bits in one raw write
    let a = x.unsigned_abs();
    let w = io.raw(cat, ((a & ((1 << (cat - 1)) - 1)) << 1) | (x < 0) as u32);
    let m = (w >> 1) | (1 << (cat - 1));
    if w & 1 != 0 { -(m as i32) } else { m as i32 }
}

/// Per-component neighbour state shared by both walks.
struct Side {
    nz7: Vec<u8>,
    nze: Vec<u8>,
    dccat: Vec<u8>,
    bottom: Vec<[i32; 8]>, // bottom boundary of the block above (per column of blocks)
}
impl Side {
    fn new(bw: usize, bh: usize) -> Self {
        Side { nz7: vec![0; bw * bh], nze: vec![0; bw * bh], dccat: vec![0; bw * bh], bottom: vec![[0; 8]; bw] }
    }
}

// ---------------------------------------------------------------- reference walk (decoder)

/// Walk one component; usable for encoding and decoding (decoder starts with zeroed blocks).
fn walk<I: Io>(io: &mut I, c: &mut Component, cc: usize, t: &Tables) {
    let (bw, bh) = (c.bw, c.bh);
    let w = &c.w;
    let mut sd = Side::new(bw, bh);
    let empty = [0i16; 64];
    let tok_base = O_TOK7 + cc * 49 * 60;
    for by in 0..bh {
        let mut right = [0i32; 8];
        for bx in 0..bw {
            let i = by * bw + bx;
            let has_a = by > 0;
            let has_l = bx > 0;
            let mut b = c.blocks[i];
            let ab = if has_a { &c.blocks[i - bw] } else { &empty };
            let lb = if has_l { &c.blocks[i - 1] } else { &empty };
            let mask = if c.masks.is_empty() { 0 } else { c.masks[i] };
            // ---- 7x7 interior
            let pa = if has_a { sd.nz7[i - bw] as u32 } else { 0 };
            let pl = if has_l { sd.nz7[i - 1] as u32 } else { 0 };
            let n7 = io.sym(O_NZ7 + cc * 50 + ((pa + pl + 1) / 2).min(49) as usize, (mask & INTERIOR_MASK).count_ones());
            sd.nz7[i] = n7 as u8;
            let mut rem = n7;
            for (k, &z) in t.interior.iter().enumerate() {
                if rem == 0 {
                    break;
                }
                let e = 2 * (ab[z].unsigned_abs() as u32 + lb[z].unsigned_abs() as u32);
                let ctx = tok_base + (k * 10 + t.e_lut[e.min(255) as usize] as usize) * 6 + t.dens_lut[rem as usize * 49 + k] as usize;
                let x = code_value(io, ctx, b[z] as i32);
                b[z] = x as i16;
                rem -= (x != 0) as u32;
            }
            // ---- edges (predictions need the interior only)
            let x = widen(&b);
            let pe = edge_preds(w, &sd.bottom[bx], &right, &col_sums(&w.wr, &x), &row_sums(&w.wc, &x));
            let pea = if has_a { sd.nze[i - bw] as u32 } else { 0 };
            let pel = if has_l { sd.nze[i - 1] as u32 } else { 0 };
            let ne = io.sym(
                O_NZE + (cc * 15 + ((pea + pel + 1) / 2).min(14) as usize) * 8 + t.n7_lut[n7 as usize] as usize,
                (mask & EDGE_MASK).count_ones(),
            );
            sd.nze[i] = ne as u8;
            let mut rem = ne;
            for ke in 0..14 {
                if rem == 0 {
                    break;
                }
                let z = edge_pos(ke);
                let avail = if ke < 7 { has_a } else { has_l };
                let hi = half_idx(pe[ke]);
                let pb = if avail { t.pe_lut[hi] as usize } else { 11 };
                let ctx = O_TOKE + ((cc * 14 + ke) * 12 + pb) * 4 + t.rem_e_lut[rem.min(15) as usize] as usize;
                let x = b[z] as i32;
                let cat = io.sym(ctx, bitlen(x));
                let mut val = 0;
                if cat > 0 {
                    let ps = if avail { sign_class(pe[ke]) } else { 3 };
                    let sctx = O_SGNE + ((cc * 14 + ke) * 4 + ps) * 6 + t.ps_lut[hi] as usize;
                    let pos = io.sym(sctx, (x > 0) as u32);
                    let m = io.raw(cat - 1, x.unsigned_abs() & ((1 << (cat - 1)) - 1)) | (1 << (cat - 1));
                    val = if pos != 0 { m as i32 } else { -(m as i32) };
                    rem -= 1;
                }
                b[z] = val as i16;
            }
            // ---- DC (needs the edges: column 0 for sr[0], row 0 for sc[0]; DC has zero weight)
            let mut sr0 = 0i32;
            let mut sc0 = 0i32;
            for k in 1..8 {
                sr0 = sr0.wrapping_add(w.wr[k * 8].wrapping_mul(b[k * 8] as i32));
                sc0 = sc0.wrapping_add(w.wc[k].wrapping_mul(b[k] as i32));
            }
            let (pred, unc) = dc_pred(w, &sd.bottom[bx], &right, sr0, sc0, has_a, has_l);
            let na = if has_a { sd.dccat[i - bw] as u32 } else { 0 };
            let nl = if has_l { sd.dccat[i - 1] as u32 } else { 0 };
            let ctx = O_DC + (cc * 9 + t.dcn_lut[(na + nl).min(63) as usize] as usize) * 8 + t.unc_lut[(unc as u32).min(127) as usize] as usize;
            let r = code_value(io, ctx, b[0] as i32 - pred);
            sd.dccat[i] = bitlen(r) as u8;
            b[0] = (r + pred) as i16;
            // ---- boundaries for neighbours
            let x = widen(&b);
            sd.bottom[bx] = col_sums(&w.wb, &x);
            right = row_sums(&w.wrt, &x);
            c.blocks[i] = b;
        }
    }
}

struct Null(u64);
impl Io for Null {
    #[inline(always)]
    fn sym(&mut self, _ctx: usize, v: u32) -> u32 {
        self.0 = self.0.wrapping_add(v as u64);
        v
    }
    #[inline(always)]
    fn raw(&mut self, _n: u32, v: u32) -> u32 {
        v
    }
}

struct BitReader<'a> {
    acc: u64,
    n: u32,
    src: &'a [u8],
    pos: usize,
}
impl<'a> BitReader<'a> {
    #[inline(always)]
    fn get(&mut self, n: u32) -> u32 {
        if self.n < n {
            let w = u32::from_le_bytes(self.src[self.pos..self.pos + 4].try_into().unwrap());
            self.pos += 4;
            self.acc |= (w as u64) << self.n;
            self.n += 32;
        }
        let v = (self.acc & ((1u64 << n) - 1)) as u32;
        self.acc >>= n;
        self.n -= n;
        v
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

struct Model {
    freq: Vec<u16>, // [N_SYM]
    cum: Vec<u16>,  // [N_SYM]
    enc: Vec<EncSym>,
}

fn normalize(hist: &[u32], want_enc: bool) -> Model {
    let mut freq = vec![0u16; N_SYM];
    let mut cum = vec![0u16; N_SYM];
    let mut enc = vec![EncSym::default(); if want_enc { N_SYM } else { 0 }];
    for c in 0..N_CTX {
        let (lo, hi) = (CTX_BASE[c] as usize, CTX_BASE[c + 1] as usize);
        let h = &hist[lo..hi];
        let tot: u64 = h.iter().map(|&x| x as u64).sum();
        let f = &mut freq[lo..hi];
        if tot == 0 {
            f[0] = PROB_SCALE as u16;
        } else {
            let mut s = 0i64;
            let mut best = 0;
            for a in 0..h.len() {
                if h[a] > 0 {
                    f[a] = ((h[a] as u64 * PROB_SCALE as u64 / tot) as u16).max(1);
                    s += f[a] as i64;
                    if h[a] > h[best] {
                        best = a;
                    }
                }
            }
            let mut d = PROB_SCALE as i64 - s;
            if d >= 0 || f[best] as i64 + d >= 1 {
                f[best] = (f[best] as i64 + d) as u16;
            } else {
                while d < 0 {
                    for a in 0..f.len() {
                        if f[a] > 1 && d < 0 {
                            f[a] -= 1;
                            d += 1;
                        }
                    }
                }
            }
        }
        let mut s = 0u32;
        for a in lo..hi {
            cum[a] = s as u16;
            let fr = freq[a] as u32;
            if want_enc && fr > 0 {
                // ryg rans64 reciprocal setup
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
                    let x0 = fr as u64 - 1;
                    let x1 = 1u64 << (shift + 31);
                    let t1 = x1 / fr as u64;
                    let x0b = x0 + ((x1 % fr as u64) << 32);
                    let t0 = x0b / fr as u64;
                    e.rcp_freq = t0 + (t1 << 32);
                    e.rcp_shift = (shift - 1) as u16;
                    e.bias = s;
                }
            }
            s += fr;
        }
    }
    Model { freq, cum, enc }
}

fn table_bytes(m: &Model, hist: &[u32]) -> Vec<u8> {
    let mut raw = Vec::new();
    for c in 0..N_CTX {
        let (lo, hi) = (CTX_BASE[c] as usize, CTX_BASE[c + 1] as usize);
        let used = hist[lo..hi].iter().any(|&x| x > 0);
        raw.push(used as u8);
        if used {
            for a in lo..hi {
                raw.extend_from_slice(&m.freq[a].to_le_bytes());
            }
        }
    }
    zstd::bulk::compress(&raw, 19).unwrap()
}

// ---------------------------------------------------------------- rANS (64-bit state, 32-bit IO)
#[inline(always)]
fn rans_put(x: &mut u64, e: &EncSym, out: &mut Vec<u32>) {
    let xmax = ((RANS_L >> PROB_BITS) << 32) * (PROB_SCALE - e.cmpl_freq as u32) as u64;
    if *x >= xmax {
        out.push(*x as u32);
        *x >>= 32;
    }
    let q = ((*x as u128 * e.rcp_freq as u128) >> 64) as u64 >> e.rcp_shift;
    *x = *x + e.bias as u64 + q * e.cmpl_freq as u64;
}

fn rans_encode(syms: &[u32], m: &Model) -> Vec<u32> {
    const _: () = assert!(NS == 4);
    let mut out: Vec<u32> = Vec::with_capacity(syms.len() / 2);
    // symbol j uses state j % 4; encode back to front with the states as plain locals
    let (mut x0, mut x1, mut x2, mut x3) = (RANS_L, RANS_L, RANS_L, RANS_L);
    let n = syms.len();
    let full = n & !3;
    for j in (full..n).rev() {
        let e = &m.enc[syms[j] as usize];
        match j & 3 {
            0 => rans_put(&mut x0, e, &mut out),
            1 => rans_put(&mut x1, e, &mut out),
            _ => rans_put(&mut x2, e, &mut out),
        }
    }
    for g in syms[..full].chunks_exact(4).rev() {
        rans_put(&mut x3, &m.enc[g[3] as usize], &mut out);
        rans_put(&mut x2, &m.enc[g[2] as usize], &mut out);
        rans_put(&mut x1, &m.enc[g[1] as usize], &mut out);
        rans_put(&mut x0, &m.enc[g[0] as usize], &mut out);
    }
    for x in [x3, x2, x1, x0] {
        out.push(x as u32);
        out.push((x >> 32) as u32);
    }
    out.reverse();
    out
}

struct Decoder<'a> {
    x: [u64; NS],
    j: usize,
    src: &'a [u32],
    pos: usize,
    m: &'a Model,
    raw: BitReader<'a>,
}
impl<'a> Io for Decoder<'a> {
    #[inline(always)]
    fn sym(&mut self, ctx: usize, _v: u32) -> u32 {
        let xs = &mut self.x[self.j % NS];
        self.j += 1;
        let slot = (*xs & (PROB_SCALE as u64 - 1)) as u16;
        let base = CTX_BASE[ctx] as usize;
        let a = alphabet(ctx);
        let cm = &self.m.cum[base..base + a];
        let s = cm[1..].iter().filter(|&&c| c <= slot).count();
        let f = self.m.freq[base + s] as u64;
        *xs = f * (*xs >> PROB_BITS) + slot as u64 - cm[s] as u64;
        if *xs < RANS_L {
            *xs = (*xs << 32) | self.src[self.pos] as u64;
            self.pos += 1;
        }
        s as u32
    }
    #[inline(always)]
    fn raw(&mut self, n: u32, _v: u32) -> u32 {
        if n == 0 { 0 } else { self.raw.get(n) }
    }
}


// ---------------------------------------------------------------- encoder-only walk
//
// Same contexts as `walk`, but the encoder knows every value up front, so all data-dependent
// branches (zero/nonzero, sign present, bit-writer flush) are replaced by arithmetic, and the
// boundary sums are done once per block.

/// Branchless LSB-first bit writer (same bitstream as `BitWriter`).
struct FastBits {
    acc: u64,
    n: u32, // < 8 between puts
    out: Vec<u8>,
    len: usize,
}
impl FastBits {
    fn with_capacity(bytes: usize) -> Self {
        FastBits { acc: 0, n: 0, out: vec![0u8; bytes + 16], len: 0 }
    }
    /// `v` must fit in `n` bits, n <= 32. Caller guarantees capacity (`reserve`).
    #[inline(always)]
    fn put(&mut self, n: u32, v: u32) {
        self.acc |= (v as u64) << self.n;
        self.n += n;
        unsafe {
            (self.out.as_mut_ptr().add(self.len) as *mut u64).write_unaligned(self.acc.to_le());
        }
        let bytes = self.n >> 3;
        self.len += bytes as usize;
        self.acc = if bytes == 8 { 0 } else { self.acc >> (bytes * 8) };
        self.n &= 7;
    }
    #[inline(always)]
    fn reserve(&mut self, bytes: usize) {
        if self.len + bytes + 16 > self.out.len() {
            self.out.resize((self.out.len() * 2).max(self.len + bytes + 16), 0);
        }
    }
    fn finish(mut self) -> Vec<u8> {
        self.out.truncate(self.len);
        self.out.extend_from_slice(&self.acc.to_le_bytes());
        self.out.extend_from_slice(&[0; 8]);
        self.out
    }
}

struct EncSink {
    syms: Vec<u32>,
    nsym: usize,
    hist: Vec<u32>,
    raw: FastBits,
}
impl EncSink {
    #[inline(always)]
    fn reserve_block(&mut self) {
        // per block at most 1 + 49 + 1 + 14 + 14 + 1 symbols and < 80 * 16 raw bits
        if self.nsym + 96 > self.syms.len() {
            self.syms.resize((self.syms.len() * 2).max(self.nsym + 96), 0);
        }
        self.raw.reserve(200);
    }
    #[inline(always)]
    fn sym(&mut self, ctx: usize, v: u32) {
        let idx = CTX_BASE[ctx] + v;
        unsafe { *self.syms.get_unchecked_mut(self.nsym) = idx };
        self.nsym += 1;
        self.hist[idx as usize] += 1;
    }
    /// symbol by precomputed compact index (CTX_BASE[ctx] + v); idx must be < N_SYM
    #[inline(always)]
    fn sym_idx(&mut self, idx: u32) {
        debug_assert!((idx as usize) < N_SYM);
        unsafe {
            *self.syms.get_unchecked_mut(self.nsym) = idx;
            *self.hist.get_unchecked_mut(idx as usize) += 1;
        }
        self.nsym += 1;
    }
    /// symbol emitted only if `on` (branchless)
    #[inline(always)]
    fn sym_if(&mut self, ctx: usize, v: u32, on: bool) {
        let idx = CTX_BASE[ctx] + v;
        unsafe { *self.syms.get_unchecked_mut(self.nsym) = idx };
        self.nsym += on as usize;
        self.hist[idx as usize] += on as u32;
    }
}

/// cat (bit length of |x|) and the raw payload written by `code_value`: (extra << 1) | sign.
#[inline(always)]
fn split_value(x: i32) -> (u32, u32) {
    let a = x.unsigned_abs();
    let cat = 32 - a.leading_zeros();
    let half = (1u32 << cat) >> 1; // 2^(cat-1), or 0
    (cat, ((a & half.wrapping_sub(1)) << 1) | (x < 0) as u32)
}

#[inline(never)]
fn walk_enc(e: &mut EncSink, c: &Component, cc: usize, t: &Tables) {
    let (bw, bh) = (c.bw, c.bh);
    let w = &c.w;
    let mut sd = Side::new(bw, bh);
    let empty = [0i16; 64];
    let tok_base = O_TOK7 + cc * 49 * 60;
    for by in 0..bh {
        let mut right = [0i32; 8];
        for bx in 0..bw {
            let i = by * bw + bx;
            e.reserve_block();
            let has_a = by > 0;
            let has_l = bx > 0;
            let b = &c.blocks[i];
            let ab = if has_a { &c.blocks[i - bw] } else { &empty };
            let lb = if has_l { &c.blocks[i - 1] } else { &empty };
            let mask = c.masks[i];
            // ---- 7x7 interior
            let pa = if has_a { sd.nz7[i - bw] as u32 } else { 0 };
            let pl = if has_l { sd.nz7[i - 1] as u32 } else { 0 };
            let n7 = (mask & INTERIOR_MASK).count_ones();
            e.sym(O_NZ7 + cc * 50 + ((pa + pl + 1) / 2).min(49) as usize, n7);
            sd.nz7[i] = n7 as u8;
            // compact index of the TOK7 family is linear: CTX_BASE[ctx] = base + (ctx - O_TOK7) * 16
            let mut rem = n7;
            let mut drow = n7 as usize * 49;
            let mut kbase = CTX_BASE[tok_base] as usize; // + k * 60 * 16
            let mut k = 0;
            // SAFETY (unchecked indexing below): k < 49 while rem > 0 (rem counts the remaining
            // nonzeros among the 49 interior positions), interior[k] < 64, ea.min(255) < 256,
            // drow + k < 50 * 49 (drow = rem * 49, rem <= 49), and idx < N_SYM by construction.
            while rem > 0 {
                let (x, ea) = unsafe {
                    let z = *t.interior.get_unchecked(k);
                    (
                        *b.get_unchecked(z) as i32,
                        2 * (ab.get_unchecked(z).unsigned_abs() as u32 + lb.get_unchecked(z).unsigned_abs() as u32),
                    )
                };
                let (cat, payload) = split_value(x);
                let idx = unsafe {
                    kbase + *t.e6_lut.get_unchecked(ea.min(255) as usize) as usize + *t.dens16_lut.get_unchecked(drow + k) as usize + cat as usize
                };
                e.sym_idx(idx as u32);
                e.raw.put(cat, payload);
                let nz = (x != 0) as u32;
                rem -= nz;
                drow -= 49 * nz as usize;
                kbase += 60 * 16;
                k += 1;
            }
            // ---- boundary sums, once for the whole block
            let x = widen(b);
            let sr = col_sums(&w.wr, &x);
            let sc = row_sums(&w.wc, &x);
            let pe = edge_preds(w, &sd.bottom[bx], &right, &sr, &sc);
            // ---- edges
            let pea = if has_a { sd.nze[i - bw] as u32 } else { 0 };
            let pel = if has_l { sd.nze[i - 1] as u32 } else { 0 };
            let ne = (mask & EDGE_MASK).count_ones();
            e.sym(O_NZE + (cc * 15 + ((pea + pel + 1) / 2).min(14) as usize) * 8 + t.n7_lut[n7 as usize] as usize, ne);
            sd.nze[i] = ne as u8;
            let mut rem = ne;
            let mut ke = 0;
            while rem > 0 {
                let z = edge_pos(ke);
                let avail = if ke < 7 { has_a } else { has_l };
                let hi = half_idx(pe[ke]);
                let pb = if avail { t.pe_lut[hi] as usize } else { 11 };
                let ctx = O_TOKE + ((cc * 14 + ke) * 12 + pb) * 4 + t.rem_e_lut[rem.min(15) as usize] as usize;
                let x = b[z] as i32;
                let a = x.unsigned_abs();
                let cat = 32 - a.leading_zeros();
                e.sym(ctx, cat);
                let ps = if avail { sign_class(pe[ke]) } else { 3 };
                let sctx = O_SGNE + ((cc * 14 + ke) * 4 + ps) * 6 + t.ps_lut[hi] as usize;
                e.sym_if(sctx, (x > 0) as u32, x != 0);
                let half = (1u32 << cat) >> 1;
                e.raw.put(cat.saturating_sub(1), a & half.wrapping_sub(1));
                rem -= (x != 0) as u32;
                ke += 1;
            }
            // ---- DC
            let (pred, unc) = dc_pred(w, &sd.bottom[bx], &right, sr[0], sc[0], has_a, has_l);
            let na = if has_a { sd.dccat[i - bw] as u32 } else { 0 };
            let nl = if has_l { sd.dccat[i - 1] as u32 } else { 0 };
            let ctx = O_DC + (cc * 9 + t.dcn_lut[(na + nl).min(63) as usize] as usize) * 8 + t.unc_lut[(unc as u32).min(127) as usize] as usize;
            let (cat, payload) = split_value(b[0] as i32 - pred);
            e.sym(ctx, cat);
            e.raw.put(cat, payload);
            sd.dccat[i] = cat as u8;
            // ---- boundaries for neighbours
            sd.bottom[bx] = col_sums(&w.wb, &x);
            right = row_sums(&w.wrt, &x);
        }
    }
}

// ---------------------------------------------------------------- public API

pub struct Encoded {
    pub rans: Vec<u32>,
    pub raw: Vec<u8>,
    pub tables: Vec<u8>,
    model: Model,
    pub nsym: usize,
}

impl Encoded {
    pub fn size(&self) -> usize {
        self.rans.len() * 4 + self.raw.len() + self.tables.len()
    }
}

#[derive(Default, Clone, Copy)]
pub struct Phases {
    pub walk: f64,
    pub norm: f64,
    pub rans: f64,
}

/// Quantisation table of component `ci` in natural order.
fn comp_w(j: &Jpeg, ci: usize) -> Weights {
    let qt = &j.qt[j.comps[ci].tq];
    let mut q = [0f32; 64];
    for z in 0..64 {
        q[ZZ[z]] = qt[z] as f32;
    }
    Weights::new(&q)
}

/// Encode all components. `coefs` is only borrowed mutably because the shared walk writes back
/// the (unchanged) values it reads.
pub fn encode(j: &Jpeg, coefs: &mut [Coefs], compress_tables: bool) -> (Encoded, Phases) {
    let t = Tables::new();
    let nblk: usize = coefs.iter().map(|c| c.blocks.len()).sum();
    let mut sink = EncSink {
        syms: vec![0; nblk * 24 + 96],
        nsym: 0,
        hist: vec![0; N_SYM],
        raw: FastBits::with_capacity(nblk * 8),
    };
    let t0 = Instant::now();
    for (ci, c) in coefs.iter_mut().enumerate() {
        let comp = Component { bw: c.bw, bh: c.bh, w: comp_w(j, ci), blocks: &mut c.blocks, masks: &c.masks };
        walk_enc(&mut sink, &comp, ci.min(1), &t);
    }
    let t1 = Instant::now();
    let model = normalize(&sink.hist, true);
    let tables = if compress_tables { table_bytes(&model, &sink.hist) } else { Vec::new() };
    let t2 = Instant::now();
    let rans = rans_encode(&sink.syms[..sink.nsym], &model);
    let ph = Phases {
        walk: (t1 - t0).as_secs_f64(),
        norm: (t2 - t1).as_secs_f64(),
        rans: t2.elapsed().as_secs_f64(),
    };
    (Encoded { rans, raw: sink.raw.finish(), tables, model, nsym: sink.nsym }, ph)
}

pub fn decode(j: &Jpeg, e: &Encoded) -> Vec<Coefs> {
    let t = Tables::new();
    let mut d = Decoder {
        x: [0; NS],
        j: 0,
        src: &e.rans,
        pos: 2 * NS,
        m: &e.model,
        raw: BitReader { acc: 0, n: 0, src: &e.raw, pos: 0 },
    };
    for s in 0..NS {
        d.x[s] = (e.rans[2 * s] as u64) << 32 | e.rans[2 * s + 1] as u64;
    }
    let mut out = Vec::new();
    for (ci, c) in j.comps.iter().enumerate() {
        let mut blocks = vec![[0i16; 64]; c.bw * c.bh];
        let mut comp = Component { bw: c.bw, bh: c.bh, w: comp_w(j, ci), blocks: &mut blocks, masks: &[] };
        walk(&mut d, &mut comp, ci.min(1), &t);
        out.push(Coefs { bw: c.bw, bh: c.bh, blocks, masks: Vec::new() });
    }
    out
}

/// Model walk alone (no symbol collection) -- for profiling.
pub fn walk_only(j: &Jpeg, coefs: &mut [Coefs]) -> u64 {
    let t = Tables::new();
    let mut n = Null(0);
    for (ci, c) in coefs.iter_mut().enumerate() {
        let mut comp = Component { bw: c.bw, bh: c.bh, w: comp_w(j, ci), blocks: &mut c.blocks, masks: &c.masks };
        walk(&mut n, &mut comp, ci.min(1), &t);
    }
    n.0
}
