//! Speed-oriented entropy stage ("P1" model).
//!
//! Per block (raster order, per component; blocks in zigzag order):
//! - number of nonzero AC coefficients, context = average of the above/left counts;
//! - for every AC position up to the last nonzero one: cat = bit length of |x|, context =
//!   (luma/chroma, position, bucket of remaining nonzeros); sign + extra bits raw;
//! - DC: residual of the MED predictor over the DC grid, context = neighbour residual sizes and
//!   the block's AC count.
//!
//! Static per-file histograms (two pass), 4-way interleaved 64-bit rANS with 32-bit IO.
//! Contexts are trivially cheap so the decoder is bound by the rANS / raw-bit chains.

use crate::jpeg::{Coefs, Error, Jpeg};
use std::time::Instant;

const PROB_BITS: u32 = 12;
const PROB_SCALE: u32 = 1 << PROB_BITS;
const RANS_L: u64 = 1 << 31;

// ---------------------------------------------------------------- contexts / compact indices
const N_NZ: usize = 2 * 64; // alphabet 64
const NREM: usize = 11; // remaining-nonzero classes, see `rem_class`
const N_AC: usize = 2 * 63 * NREM; // alphabet 16
const N_DC: usize = 2 * 9 * 6; // alphabet 16
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
    let sh = if b >= 2 { b - 2 } else { 0 };
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
        2 => (s + 1) / 2,
        1 => s,
        _ => 0,
    };
    NZ_BASE + (cc * 64 + p.min(63) as usize) * 64
}
#[inline(always)]
fn dc_ctx(cc: usize, cat_sum: u32, nz: u32) -> usize {
    DC_BASE + ((cc * 9 + DCN_B[cat_sum.min(63) as usize] as usize) * 6 + NZ_B[nz as usize] as usize) * 16
}
#[inline(always)]
fn med(a: i32, b: i32, c: i32) -> i32 {
    let (mx, mn) = (a.max(b), a.min(b));
    if c >= mx { mn } else if c <= mn { mx } else { a + b - c }
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
    fn finish(mut self) -> Vec<u8> {
        self.out.extend_from_slice(&self.acc.to_le_bytes());
        self.out.extend_from_slice(&[0; 8]);
        self.out
    }
}

struct Sink {
    syms: Vec<u32>,
    n: usize,
    hist: Vec<u32>,
    raw: BitWriter,
}
impl Sink {
    #[inline(always)]
    fn reserve_block(&mut self) {
        if self.n + 66 > self.syms.len() {
            self.syms.resize((self.syms.len() * 2).max(self.n + 66), 0);
        }
        self.raw.reserve(64 * 2 + 8);
    }
    #[inline(always)]
    fn sym(&mut self, idx: usize) {
        debug_assert!(idx < N_SYM);
        // SAFETY: reserve_block keeps room for a whole block; idx < N_SYM by construction
        unsafe {
            *self.syms.get_unchecked_mut(self.n) = idx as u32;
            *self.hist.get_unchecked_mut(idx) += 1;
        }
        self.n += 1;
    }
}

#[inline(never)]
fn walk_enc(s: &mut Sink, c: &Coefs, cc: usize) {
    let (bw, bh) = (c.bw, c.bh);
    let mut nzs = vec![0u8; bw * bh];
    let mut dcc = vec![0u8; bw * bh];
    let ac_idx = &AC_IDX[cc];
    for by in 0..bh {
        for bx in 0..bw {
            let i = by * bw + bx;
            s.reserve_block();
            let (has_a, has_l) = (by > 0, bx > 0);
            let b = &c.blocks[i];
            let mask = c.masks[i] & !1;
            let nz = mask.count_ones();
            let (nza, nzl) = (if has_a { nzs[i - bw] } else { 0 }, if has_l { nzs[i - 1] } else { 0 });
            s.sym(nz_ctx(cc, nza as u32, nzl as u32, has_a, has_l) + nz as usize);
            nzs[i] = nz as u8;
            if nz > 0 {
                let last = 63 - mask.leading_zeros() as usize;
                let mut rem = nz as usize;
                for k in 1..=last {
                    // SAFETY: k < 64, rem in 1..=63 while k <= last
                    let x = unsafe { *b.get_unchecked(k) } as i32;
                    let (cat, payload) = split_value(x);
                    s.sym(unsafe { *ac_idx.get_unchecked(k).get_unchecked(rem) } as usize + cat as usize);
                    s.raw.put(cat, payload);
                    rem -= (x != 0) as usize;
                }
            }
            let pred = dc_pred(&c.blocks, i, bw, bx, by);
            let (cat, payload) = split_value(b[0] as i32 - pred);
            let cs = if has_a { dcc[i - bw] as u32 } else { 0 } + if has_l { dcc[i - 1] as u32 } else { 0 };
            s.sym(dc_ctx(cc, cs, nz) + cat as usize);
            s.raw.put(cat, payload);
            dcc[i] = cat as u8;
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
        let data: f64 = (0..len).filter(|&a| h[a] > 0).map(|a| h[a] as f64 * (total as f64 / tmp[a] as f64).log2()).sum();
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
        P_DC + (lo - DC_BASE) / 16 / 6 * 16
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
    let mut gshift = vec![0u8; G_SYM / 16];
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
                let inherit: f64 = (0..len).filter(|&a| h[a] > 0).map(|a| h[a] as f64 * (PROB_SCALE as f64 / gf[a] as f64).log2()).sum();
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
        let inherit: f64 = (0..len).filter(|&a| h[a] > 0).map(|a| h[a] as f64 * (PROB_SCALE as f64 / pf[a] as f64).log2()).sum();
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
                    let t0 = (fr as u64 - 1 + ((x1 % fr as u64) << 32)) / fr as u64;
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

fn rans_encode(syms: &[u32], t: &Tables) -> Vec<u32> {
    let mut out: Vec<u32> = Vec::with_capacity(syms.len() / 2);
    let (mut x0, mut x1, mut x2, mut x3) = (RANS_L, RANS_L, RANS_L, RANS_L);
    let n = syms.len();
    let full = n & !3;
    for j in (full..n).rev() {
        let e = &t.enc[syms[j] as usize];
        match j & 3 {
            0 => rans_put(&mut x0, e, &mut out),
            1 => rans_put(&mut x1, e, &mut out),
            _ => rans_put(&mut x2, e, &mut out),
        }
    }
    for g in syms[..full].chunks_exact(4).rev() {
        rans_put(&mut x3, &t.enc[g[3] as usize], &mut out);
        rans_put(&mut x2, &t.enc[g[2] as usize], &mut out);
        rans_put(&mut x1, &t.enc[g[1] as usize], &mut out);
        rans_put(&mut x0, &t.enc[g[0] as usize], &mut out);
    }
    for x in [x3, x2, x1, x0] {
        out.push(x as u32);
        out.push((x >> 32) as u32);
    }
    out.reverse();
    out
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
fn walk_dec(d: &mut Dec, blocks: &mut [[i16; 64]], masks: &mut [u64], bw: usize, bh: usize, cc: usize) -> Result<(), Error> {
    let mut nzs = vec![0u8; bw * bh];
    let mut dcc = vec![0u8; bw * bh];
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
            let pred = dc_pred(blocks, i, bw, bx, by);
            let cs = if has_a { dcc[i - bw] as u32 } else { 0 } + if has_l { dcc[i - 1] as u32 } else { 0 };
            let cat = d.sym(dc_ctx(cc, cs, nz));
            let w = d.raw(cat);
            let dc = pred + join_value(cat, w);
            blocks[i][0] = dc as i16;
            masks[i] = mask | (dc != 0) as u64;
            dcc[i] = cat as u8;
            if d.bp > d.raw_bits || d.pos > d.src_len {
                return Err(Error("corrupt stream".into()));
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------- public API

pub struct Encoded {
    pub rans: Vec<u32>,
    pub raw: Vec<u8>,
    pub tables: Vec<u8>,
    t: Tables,
    pub nsym: usize,
}

fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push(v as u8 | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}
pub(crate) fn get_varint(d: &[u8], p: &mut usize) -> Result<u64, Error> {
    let mut v = 0u64;
    for sh in (0..64).step_by(7) {
        let b = *d.get(*p).ok_or(Error("truncated".into()))?;
        *p += 1;
        v |= ((b & 0x7F) as u64) << sh;
        if b < 0x80 {
            return Ok(v);
        }
    }
    Err(Error("bad varint".into()))
}
pub(crate) fn get_bytes<'a>(d: &'a [u8], p: &mut usize, n: u64) -> Result<&'a [u8], Error> {
    let n = usize::try_from(n).map_err(|_| Error("bad length".into()))?;
    let s = d.get(*p..p.checked_add(n).ok_or(Error("bad length".into()))?).ok_or(Error("truncated".into()))?;
    *p += n;
    Ok(s)
}

impl Encoded {
    pub fn size(&self) -> usize {
        self.rans.len() * 4 + self.raw.len() + self.tables.len()
    }
    /// Serialise: tables, rANS words, raw bits (each length-prefixed).
    pub fn write(&self, out: &mut Vec<u8>) {
        put_varint(out, self.tables.len() as u64);
        out.extend_from_slice(&self.tables);
        put_varint(out, self.rans.len() as u64);
        out.reserve(self.rans.len() * 4);
        if cfg!(target_endian = "little") {
            // SAFETY: u32 has no padding; on little-endian targets its bytes are the LE encoding
            out.extend_from_slice(unsafe { std::slice::from_raw_parts(self.rans.as_ptr() as *const u8, self.rans.len() * 4) });
        } else {
            for w in &self.rans {
                out.extend_from_slice(&w.to_le_bytes());
            }
        }
        put_varint(out, self.raw.len() as u64);
        out.extend_from_slice(&self.raw);
    }
    pub fn read(d: &[u8], p: &mut usize) -> Result<Encoded, Error> {
        let n = get_varint(d, p)?;
        let tables = get_bytes(d, p, n)?.to_vec();
        let t = build_tables(read_tables(&tables)?, false);
        let n = get_varint(d, p)?;
        let words = get_bytes(d, p, n.checked_mul(4).ok_or(Error("bad length".into()))?)?;
        let mut rans = vec![0u32; words.len() / 4];
        if cfg!(target_endian = "little") {
            // SAFETY: as in `write`; the destination has exactly words.len() bytes
            unsafe { std::ptr::copy_nonoverlapping(words.as_ptr(), rans.as_mut_ptr() as *mut u8, words.len()) };
        } else {
            for (r, c) in rans.iter_mut().zip(words.chunks_exact(4)) {
                *r = u32::from_le_bytes(c.try_into().unwrap());
            }
        }
        if rans.len() < 8 {
            return Err(Error("truncated rANS stream".into()));
        }
        let n = get_varint(d, p)?;
        let raw = get_bytes(d, p, n)?.to_vec();
        Ok(Encoded { rans, raw, tables, t, nsym: 0 })
    }
}

pub use crate::model::Phases;

thread_local! {
    /// symbol and histogram buffers, reused across calls
    static BUFS: std::cell::RefCell<(Vec<u32>, Vec<u32>)> = const { std::cell::RefCell::new((Vec::new(), Vec::new())) };
}

/// `coefs` must be in zigzag order (`jpeg::Order::Zigzag`), with nonzero masks.
pub fn encode(_j: &Jpeg, coefs: &[Coefs]) -> (Encoded, Phases) {
    let nblk: usize = coefs.iter().map(|c| c.blocks.len()).sum();
    let (mut syms, mut hist) = BUFS.with(|b| std::mem::take(&mut *b.borrow_mut()));
    if syms.len() < nblk * 16 + 66 {
        syms.resize(nblk * 16 + 66, 0);
    }
    hist.clear();
    hist.resize(N_SYM, 0);
    let mut s = Sink { syms, n: 0, hist, raw: BitWriter::with_capacity(nblk * 8) };
    let t0 = Instant::now();
    for (ci, c) in coefs.iter().enumerate() {
        walk_enc(&mut s, c, ci.min(1));
    }
    let t1 = Instant::now();
    let (freq, plan) = normalize(&s.hist);
    let tables = write_tables(&freq, &plan, &s.hist);
    let t = build_tables(freq, true);
    let t2 = Instant::now();
    let rans = rans_encode(&s.syms[..s.n], &t);
    let ph = Phases { walk: (t1 - t0).as_secs_f64(), norm: (t2 - t1).as_secs_f64(), rans: t2.elapsed().as_secs_f64() };
    let nsym = s.n;
    BUFS.with(|b| *b.borrow_mut() = (s.syms, s.hist));
    (Encoded { rans, raw: s.raw.finish(), tables, t, nsym }, ph)
}

/// Decodes into zigzag-ordered blocks, with nonzero masks.
pub fn decode(j: &Jpeg, e: &Encoded) -> Result<Vec<Coefs>, Error> {
    let mut out = Vec::new();
    decode_into(j, e, &mut out)?;
    Ok(out)
}

/// Like `decode`, reusing the allocations in `out`.
pub fn decode_into(j: &Jpeg, e: &Encoded, out: &mut Vec<Coefs>) -> Result<(), Error> {
    // Reads of a corrupt stream may run up to one block past the end before `walk_dec` notices:
    // pad the rANS words (<= 1 per symbol, <= 65 symbols per block) and the raw bits.
    let mut src = Vec::with_capacity(e.rans.len() + SRC_PAD);
    src.extend_from_slice(&e.rans);
    src.resize(e.rans.len() + SRC_PAD, 0);
    let mut raw = Vec::with_capacity(e.raw.len() + RAW_PAD);
    raw.extend_from_slice(&e.raw);
    raw.resize(e.raw.len() + RAW_PAD, 0);
    let mut d = Dec { x: [0; 4], src: &src, pos: 8, t: &e.t, raw: &raw, bp: 0, raw_bits: e.raw.len() * 8, src_len: e.rans.len() };
    for s in 0..4 {
        d.x[s] = (e.rans[2 * s] as u64) << 32 | e.rans[2 * s + 1] as u64;
    }
    out.resize_with(j.comps.len(), || Coefs { bw: 0, bh: 0, blocks: Vec::new(), masks: Vec::new() });
    for (ci, (c, o)) in j.comps.iter().zip(out.iter_mut()).enumerate() {
        o.bw = c.bw;
        o.bh = c.bh;
        o.blocks.resize(c.bw * c.bh, [0; 64]);
        o.masks.resize(c.bw * c.bh, 0);
        walk_dec(&mut d, &mut o.blocks, &mut o.masks, c.bw, c.bh, ci.min(1))?;
    }
    Ok(())
}
