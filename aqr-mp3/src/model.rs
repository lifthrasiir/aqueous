//! The coefficient model. One code path serves both directions: `c.sym(ctx, s)` codes `s`
//! when compressing and returns the decoded symbol when decompressing.

use crate::frame::{Granule, Header, PRETAB, SFB_LONG, SideInfo, full_scalefactors, scf_layout};
use crate::huff;
use crate::huff_tables::{DIM, LINBITS};

/// Codes symbols and raw bits. One code path serves both directions: `sym(ctx, s)` codes
/// `s` when compressing and returns the decoded symbol when decompressing.
pub trait Sym {
    /// Codes symbol `s` in context `ctx` (compressing), or decodes one (decompressing).
    fn sym(&mut self, ctx: usize, s: u32) -> u32;
    /// Codes the low `n` bits of `v` verbatim (n <= 32).
    fn raw(&mut self, n: u32, v: u32) -> u32;
}

/// A coder. Granules are coded through its [`Coder::local`] state, a value that the
/// compiler can keep in registers, which is handed back with [`Coder::put_back`].
pub trait Coder {
    type L: Sym;
    fn local(&mut self) -> Self::L;
    fn put_back(&mut self, l: Self::L);
    /// Called before a granule with bounds on what it will code.
    #[inline(always)]
    fn reserve(&mut self, _syms: usize, _raw_bits: usize) {}
    /// Whether the coder can return to a saved point, so that granules may be decoded
    /// and coded in one pass ([`encode_granule`]).
    const ROLLBACK: bool = false;
    fn save(&self) -> [u64; 4] {
        [0; 4]
    }
    fn restore(&mut self, _s: [u64; 4]) {}
    /// Called before every frame whose index is a positive multiple of
    /// [`crate::codec::RUN_FRAMES`]: the rANS stream starts a new run.
    #[inline(always)]
    fn restart(&mut self) {}
}

/// At most this many symbols and raw bits per granule.
pub const MAX_GRANULE_SYMS: usize = 3 * 288 + 144;
pub const MAX_GRANULE_RAW_BITS: usize = 288 * 2 * 13 + 576;

/// Magnitude buckets of the pair context, plus one for "no neighbourhood".
pub const NB: usize = 24;
pub const NBX: usize = NB + 1;
/// Position classes of the pair context (64 lines each, the last one open).
pub const NPOS: usize = 1;
/// Pair contexts: (table_select, magnitude bucket, position class). The magnitude is the
/// neighbourhood in the previous granule (scaled to the current step sizes) and in the
/// other channel, so the contexts of a granule depend on nothing decoded in it.
pub const PAIR_BASE: usize = 0;
pub const PAIR_PER_TABLE: usize = NBX * NOC * NPOS * NCUR;
/// Classes of the other channel's magnitude at the same lines (0: none).
pub const NOC: usize = 6;
/// Classes of the magnitude of the pair `LAG` pairs back in the same granule.
pub const NCUR: usize = 1;
const LAG: usize = 2;

#[inline(always)]
fn cur_class(cur: &[u16; 576], line: usize) -> usize {
    if line < 2 * LAG {
        return 0;
    }
    if NCUR == 1 {
        return 0;
    }
    let s = cur[line - 2 * LAG] as u32 + cur[line - 2 * LAG + 1] as u32;
    (s > 0) as usize + (s > 2) as usize
}
pub const LIN_BASE: usize = PAIR_BASE + 32 * PAIR_PER_TABLE;
/// Escape contexts: (table_select - 16, magnitude bucket capped at 15). The symbol is
/// `bit length << 1 | the bit below the top one` (0 for lengths below 2).
pub const LIN_ALPHA: usize = 28;
pub const QUAD_BASE: usize = LIN_BASE + 16 * 16;
/// Quadruple contexts: (count1 table, nonzero values at the same lines of the previous
/// granule 0..=4 or 5 if none, position class).
pub const QUAD_PER_TABLE: usize = 6 * 3 * NPOS;
/// Scalefactor contexts: (size in bits 1..=5, previous scalefactor of the granule capped
/// at 15 or 16 if none, same scalefactor of the previous coded granule of the channel
/// capped at 15 or 16 if none).
pub const SCF_BASE: usize = QUAD_BASE + 2 * QUAD_PER_TABLE;
pub const N_CTX: usize = SCF_BASE + 5 * 17 * 17;

/// Alphabet size of each context.
pub fn alphabet(ctx: usize) -> usize {
    if ctx < LIN_BASE {
        let ts = (ctx - PAIR_BASE) / PAIR_PER_TABLE;
        let d = DIM[ts] as usize;
        (d * d).max(1)
    } else if ctx < QUAD_BASE {
        LIN_ALPHA
    } else if ctx < SCF_BASE {
        16
    } else {
        1 << ((ctx - SCF_BASE) / (17 * 17) + 1)
    }
}

/// Contexts of the same family share an alphabet and serve as each other's prior when
/// training (pairs and escapes: same table; quadruples: same count1 table; scalefactors:
/// same size).
pub fn family(ctx: usize) -> usize {
    if ctx < LIN_BASE {
        (ctx - PAIR_BASE) / PAIR_PER_TABLE
    } else if ctx < QUAD_BASE {
        32 + (ctx - LIN_BASE) / 16
    } else if ctx < SCF_BASE {
        48 + (ctx - QUAD_BASE) / QUAD_PER_TABLE
    } else {
        50 + (ctx - SCF_BASE) / (17 * 17)
    }
}

/// Codes the coded scalefactors of a granule (`vals`, bitstream order).
pub fn code_scalefactors<C: Coder>(
    c: &mut C,
    st: &mut State,
    lay: &crate::frame::ScfLayout,
    ch: usize,
    vals: &mut [u8],
) {
    let mut l = c.local();
    let p = &mut st.scf_prev[ch];
    let (mut i, mut k) = (0usize, 0usize);
    let mut left = 16usize;
    for g in 0..4 {
        let (n, size) = (lay.count[g] as usize, lay.size[g] as usize);
        if lay.copy[g] {
            i += n;
            continue;
        }
        if size == 0 {
            vals[k..k + n].fill(0);
            i += n;
            k += n;
            continue;
        }
        debug_assert!(size <= 5);
        let base = SCF_BASE + (size - 1) * 17 * 17;
        for _ in 0..n {
            let ix = i.min(38);
            let v = l.sym(base + left * 17 + p[ix] as usize, vals[k] as u32) & ((1 << size) - 1);
            vals[k] = v as u8;
            p[ix] = v.min(15) as u8;
            left = v.min(15) as usize;
            i += 1;
            k += 1;
        }
    }
    c.put_back(l);
}

/// Buckets per octave of the magnitude buckets (2 or 4).
const BPO: u32 = 2;

/// Logarithmic magnitude bucket: v itself below 4, then `BPO` per octave, capped.
const fn bucket_of(v: u32) -> u32 {
    if v < 4 {
        return v;
    }
    let l = 31 - v.leading_zeros();
    let frac = (v >> (l - 2)) & 3;
    let b = BPO * l + (frac >> (2 - BPO.trailing_zeros())) - (2 * BPO - 4);
    if b > NB as u32 - 1 { NB as u32 - 1 } else { b }
}

#[inline(always)]
fn lb_calc(v: u32) -> u32 {
    bucket_of(v)
}

const LB_N: usize = 4096;
static LB: [u8; LB_N] = {
    let mut t = [0u8; LB_N];
    let mut v = 0;
    while v < LB_N {
        t[v] = bucket_of(v as u32) as u8;
        v += 1;
    }
    t
};

#[inline(always)]
fn lb(v: u32) -> u32 {
    if (v as usize) < LB_N { LB[v as usize] as u32 } else { lb_calc(v) }
}

/// 2^(r/16) in 1/65536 units.
const POW16: [u32; 16] = [
    65536, 68438, 71468, 74632, 77936, 81386, 84990, 88752, 92682, 96785, 101070, 105545, 110218, 115098, 120194,
    125515,
];

/// 2^(-3d/16) in 1/256 units, for the step exponent difference d (clamped to ±80): how much
/// the quantized magnitudes of the previous granule change with the current step size.
fn step_factor(d: i32) -> u64 {
    let e = 128 - 3 * d.clamp(-80, 80); // 2^(e/16)
    let (q, r) = (e.div_euclid(16), e.rem_euclid(16));
    let m = POW16[r as usize] as u64;
    if q >= 16 { m << (q - 16) } else { m >> (16 - q) }
}

/// Symbol index -> x << 4 | y, for each pair alphabet dimension.
static XY: [[u8; 256]; 17] = {
    let mut t = [[0u8; 256]; 17];
    let mut d = 1;
    while d <= 16 {
        let mut s = 0;
        while s < d * d {
            t[d][s] = ((s / d) << 4 | (s % d)) as u8;
            s += 1;
        }
        d += 1;
    }
    t
};

pub struct State {
    gr0_scf: [[u8; 39]; 2],
    exp: [[i16; 22]; 2],
    /// Magnitudes of the previous granule per channel, from index 1 (index 0 and the
    /// end are zero padding).
    prev: [[u16; 580]; 2],
    prev_ok: [bool; 2],
    /// Magnitudes of channel 0 of the current granule.
    cur0: [u16; 576],
    /// Last coded value of each scalefactor index per channel (capped at 15; 16 = none).
    pub scf_prev: [[u8; 39]; 2],
}

impl State {
    pub fn new() -> Box<State> {
        Box::new(State {
            gr0_scf: [[0; 39]; 2],
            exp: [[0; 22]; 2],
            prev: [[0; 580]; 2],
            prev_ok: [false; 2],
            cur0: [0; 576],
            scf_prev: [[16; 39]; 2],
        })
    }
}

#[inline(always)]
#[allow(clippy::unnecessary_min_or_max)] // NPOS is a tuning constant (currently one class)
fn pos_class(line: usize) -> usize {
    (line >> 7).min(NPOS - 1)
}

/// Quantizer step exponents of the long bands of a long-block granule, in quarter powers of
/// two (larger = coarser): global_gain - 2 or 4 × (scalefactor + pre-emphasis).
fn band_exponents(g: &Granule, scf: &[u8; 39]) -> [i16; 22] {
    let mult = if g.scalefac_scale { 4 } else { 2 };
    let mut e = [0i16; 22];
    for b in 0..22 {
        let sf = scf[b] as i32 + if g.preflag { PRETAB[b] as i32 } else { 0 };
        e[b] = (g.global_gain as i32 - mult * sf) as i16;
    }
    e
}

/// Contexts of a granule, computed before coding it; the state is updated by [`commit`].
struct Prep {
    /// Magnitude bucket per pair (NB: none).
    bucket: [u8; 288],
    /// Step exponents of the long bands (see [`band_exponents`]).
    exp: [i16; 22],
    full: [u8; 39],
    use_prev: bool,
    short: bool,
    other: bool,
}

fn prepare(st: &State, h: Header, si: &SideInfo, gr: usize, ch: usize, scf: &[u8]) -> Prep {
    let g = &si.gr[gr][ch];
    let short = g.short();
    let use_prev = !short && st.prev_ok[ch];
    let other = ch == 1 && h.channels() == 2;
    let npairs = (g.big_values as usize).min(288);

    // Per pair: weighted neighbourhood magnitude from the previous granule (scaled to the
    // current step sizes) and the other channel.
    let mut nbh = [0u32; 288];
    let lay = scf_layout(h, si, gr, ch);
    let full = full_scalefactors(&lay, scf, &st.gr0_scf[ch]);
    let exp = if short { [0; 22] } else { band_exponents(g, &full) };
    if use_prev {
        let p = &st.prev[ch];
        let pe = &st.exp[ch];
        let widths = &SFB_LONG[h.sfb_row()];
        let (mut line, mut i) = (0usize, 0usize);
        for b in 0..22 {
            if i >= npairs {
                break;
            }
            let f = step_factor((exp[b] - pe[b]) as i32);
            line += widths[b] as usize;
            let stop = (line / 2).min(npairs);
            while i < stop {
                let k = 2 * i;
                let (pm, p0, p1, p2) = (p[k] as u64, p[k + 1] as u64, p[k + 2] as u64, p[k + 3] as u64);
                let wide = if k >= 1 { p[k - 1] as u64 } else { 0 } + p[(k + 4).min(579)] as u64;
                nbh[i] = (((4 * (p0 + p1) + 2 * (pm + p2) + wide) * f) >> 9).min(1 << 24) as u32;
                i += 1;
            }
        }
    }
    let mut oc = [0u8; 288];
    if other {
        for i in 0..npairs {
            let o = st.cur0[2 * i] as u32 + st.cur0[2 * i + 1] as u32;
            oc[i] = 1 + (o > 0) as u8 + (o > 2) as u8 + (o > 6) as u8 + (o > 14) as u8;
        }
    }
    let w = if use_prev { 7 } else { 0 };
    let mut bucket = [NB as u8; 288];
    if w > 0 {
        let recip = (16u64 << 16).div_ceil(w as u64);
        for i in 0..npairs {
            bucket[i] = lb(((nbh[i] as u64 * recip) >> 16) as u32) as u8;
        }
    }
    for i in 0..npairs {
        bucket[i] = bucket[i] * NOC as u8 + oc[i];
    }
    Prep { bucket, exp, full, use_prev, short, other }
}

/// Quadruple context from the other channel: 0 none, 1 all zero, 2 some nonzero.
#[inline(always)]
fn quad_other(p: &Prep, st: &State, line: usize) -> usize {
    if !p.other {
        return 0;
    }
    let o = &st.cur0[line..line + 4];
    1 + (o[0] | o[1] | o[2] | o[3] != 0) as usize
}

/// Updates the state after a granule with magnitudes `cur`.
fn commit(st: &mut State, p: &Prep, gr: usize, ch: usize, cur: &[u16; 576]) {
    if gr == 0 {
        st.gr0_scf[ch] = p.full;
    }
    st.exp[ch] = p.exp;
    st.prev[ch][1..577].copy_from_slice(cur);
    st.prev_ok[ch] = !p.short;
    if ch == 0 {
        st.cur0 = *cur;
    }
}

/// Codes the quantized values of one granule and channel (big values per the side
/// information, then `quads` count1 quadruples; the rest is zero).
#[inline(never)]
#[allow(clippy::too_many_arguments)] // the hot path, kept flat
pub fn code_granule<C: Coder>(
    c: &mut C,
    st: &mut State,
    h: Header,
    si: &SideInfo,
    gr: usize,
    ch: usize,
    scf: &[u8],
    quads: usize,
    coefs: &mut [i32; 576],
) {
    c.reserve(MAX_GRANULE_SYMS, MAX_GRANULE_RAW_BITS);
    let mut l = c.local();
    let g = &si.gr[gr][ch];
    let prep = prepare(st, h, si, gr, ch, scf);
    let use_prev = prep.use_prev;
    let bucket = &prep.bucket;

    // Magnitudes of this granule.
    let mut cur = [0u16; 576];
    let reg = huff::regions(h, g);
    let mut line = 0usize;
    for t in 0..3 {
        let ts = g.table_select[t] as usize;
        let n = reg[t];
        if ts == 0 {
            coefs[line..line + 2 * n].fill(0);
            line += 2 * n;
            continue;
        }
        let dim = DIM[ts] as usize;
        let xy = &XY[dim];
        let lin = LINBITS[ts] as u32;
        let base = PAIR_BASE + ts * PAIR_PER_TABLE;
        for _ in 0..n {
            let i = line / 2;
            let bv = bucket[i] as usize;
            let ctx = base + (bv * NPOS + pos_class(line)) * NCUR + cur_class(&cur, line);
            let (a, b) = (coefs[line], coefs[line + 1]);
            let (x, y) = (a.unsigned_abs().min(15) as usize, b.unsigned_abs().min(15) as usize);
            let sym = l.sym(ctx, (x * dim + y) as u32);
            let xy = xy[sym as usize & 255] as u32;
            let (mut x, mut y) = (xy >> 4, xy & 15);
            if lin > 0 && (x == 15 || y == 15) {
                let lctx = LIN_BASE + (ts - 16) * 16 + (bv / NOC).min(15);
                for (m, v0) in [(&mut x, a), (&mut y, b)] {
                    if *m == 15 {
                        let e = v0.unsigned_abs().wrapping_sub(15);
                        let bl = 32 - e.leading_zeros();
                        let top = if bl >= 2 { e >> (bl - 2) & 1 } else { 0 };
                        let s = l.sym(lctx, bl << 1 | top);
                        let (bl, top) = (s >> 1, s & 1);
                        let e = match bl {
                            0 => 0,
                            1 => 1,
                            _ => ((1u32 << (bl - 1)) | top << (bl - 2) | l.raw(bl - 2, e)) & ((1 << lin) - 1),
                        };
                        *m = 15 + e;
                    }
                }
            }
            // Signs of the nonzero values, in one go.
            let nz = (x != 0) as u32 + (y != 0) as u32;
            let sbits =
                (((a < 0) as u32) << (y != 0) as u32 & ((x != 0) as u32) << (y != 0) as u32) | (y != 0 && b < 0) as u32;
            let sbits = l.raw(nz, sbits);
            let sy = y != 0 && sbits & 1 != 0;
            let sx = x != 0 && (sbits >> (nz - (x != 0) as u32)) & 1 != 0;
            coefs[line] = if sx { -(x as i32) } else { x as i32 };
            coefs[line + 1] = if sy { -(y as i32) } else { y as i32 };
            cur[line] = x.min(u16::MAX as u32) as u16;
            cur[line + 1] = y.min(u16::MAX as u32) as u16;
            line += 2;
        }
    }
    let qbase = QUAD_BASE + if g.count1table { QUAD_PER_TABLE } else { 0 };
    let p = &st.prev[ch];
    for _ in 0..quads {
        let pg = if use_prev {
            (p[line + 1] != 0) as usize
                + (p[line + 2] != 0) as usize
                + (p[line + 3] != 0) as usize
                + (p[line + 4] != 0) as usize
        } else {
            5
        };
        let q = &coefs[line..line + 4];
        let sym = (q[0] != 0) as u32 * 8 + (q[1] != 0) as u32 * 4 + (q[2] != 0) as u32 * 2 + (q[3] != 0) as u32;
        let neg = (q[0] < 0) as u32 * 8 + (q[1] < 0) as u32 * 4 + (q[2] < 0) as u32 * 2 + (q[3] < 0) as u32;
        let sym = l.sym(qbase + (pg * 3 + quad_other(&prep, st, line)) * NPOS + pos_class(line), sym) & 15;
        // Sign bits of the nonzero values, first value in the highest bit.
        let ns = sym.count_ones();
        let packed = l.raw(ns, pext4(neg, sym));
        let neg = pdep4(packed, sym);
        for j in 0..4 {
            let nz = (sym >> (3 - j)) & 1;
            let ng = (neg >> (3 - j)) & 1;
            coefs[line + j] = nz as i32 * (1 - 2 * ng as i32);
            cur[line + j] = nz as u16;
        }
        line += 4;
    }
    coefs[line..].fill(0);
    cur[line..].fill(0);
    c.put_back(l);
    commit(st, &prep, gr, ch, &cur);
}

/// Decompressing only: decodes the values of a granule as [`code_granule`] would and writes
/// their Huffman coding (part 3 without the tail) to `w` directly.
#[inline(never)]
#[allow(clippy::too_many_arguments)] // the hot path, kept flat
pub fn decode_granule<C: Coder>(
    c: &mut C,
    st: &mut State,
    h: Header,
    si: &SideInfo,
    gr: usize,
    ch: usize,
    scf: &[u8],
    quads: usize,
    w: &mut crate::bits::FastWriter,
) {
    use crate::huff_tables::{COUNT1A, COUNT1B, PAIR};
    c.reserve(MAX_GRANULE_SYMS, MAX_GRANULE_RAW_BITS);
    let mut l = c.local();
    // At most 47 bits per pair, 10 per quadruple.
    w.reserve(288 * 6 + 144 * 2 + 16);
    let mut wl = w.local();
    let g = &si.gr[gr][ch];
    let prep = prepare(st, h, si, gr, ch, scf);
    let mut cur = [0u16; 576];
    let reg = huff::regions(h, g);
    let mut line = 0usize;
    for t in 0..3 {
        let ts = g.table_select[t] as usize;
        let n = reg[t];
        if ts == 0 {
            line += 2 * n;
            continue;
        }
        let dim = DIM[ts] as usize;
        let xy = &XY[dim];
        let codes = PAIR[ts].unwrap();
        let lin = LINBITS[ts] as u32;
        let base = PAIR_BASE + ts * PAIR_PER_TABLE;
        for _ in 0..n {
            let bv = prep.bucket[line / 2] as usize;
            let ctx = base + (bv * NPOS + pos_class(line)) * NCUR + cur_class(&cur, line);
            let sym = l.sym(ctx, 0);
            let xy = xy[sym as usize & 255] as u32;
            let (x, y) = (xy >> 4, xy & 15);
            let e = codes[(xy & 0xff) as usize];
            let nz = (x != 0) as u32 + (y != 0) as u32;
            if lin > 0 && (x == 15 || y == 15) {
                let lctx = LIN_BASE + (ts - 16) * 16 + (bv / NOC).min(15);
                let mut ext = [0u32; 2];
                for (j, &m) in [x, y].iter().enumerate() {
                    if m == 15 {
                        let s = l.sym(lctx, 0);
                        let (bl, top) = (s >> 1, s & 1);
                        ext[j] = match bl {
                            0 => 0,
                            1 => 1,
                            _ => ((1u32 << (bl - 1)) | top << (bl - 2) | l.raw(bl - 2, 0)) & ((1 << lin) - 1),
                        };
                    }
                }
                let signs = l.raw(nz, 0);
                // Code, then per value: escape (if 15) and sign (if nonzero).
                let mut bits = (e >> 5) as u64;
                let mut nb = e & 31;
                let mut k = nz;
                for (j, &m) in [x, y].iter().enumerate() {
                    if m == 15 {
                        bits = bits << lin | ext[j] as u64;
                        nb += lin;
                    }
                    if m != 0 {
                        k -= 1;
                        bits = bits << 1 | (signs >> k & 1) as u64;
                        nb += 1;
                    }
                }
                wl.put(bits, nb);
                cur[line] = (x + ext[0]).min(u16::MAX as u32) as u16;
                cur[line + 1] = (y + ext[1]).min(u16::MAX as u32) as u16;
            } else {
                // The signs follow the code directly, in the same order.
                let signs = l.raw(nz, 0);
                wl.put(((e >> 5) as u64) << nz | signs as u64, (e & 31) + nz);
                cur[line] = x as u16;
                cur[line + 1] = y as u16;
            }
            line += 2;
        }
    }
    let qbase = QUAD_BASE + if g.count1table { QUAD_PER_TABLE } else { 0 };
    let tab = if g.count1table { &COUNT1B } else { &COUNT1A };
    let p = &st.prev[ch];
    for _ in 0..quads {
        let pg = if prep.use_prev {
            (p[line + 1] != 0) as usize
                + (p[line + 2] != 0) as usize
                + (p[line + 3] != 0) as usize
                + (p[line + 4] != 0) as usize
        } else {
            5
        };
        let sym = l.sym(qbase + (pg * 3 + quad_other(&prep, st, line)) * NPOS + pos_class(line), 0) & 15;
        let ns = sym.count_ones();
        let signs = l.raw(ns, 0);
        let e = tab[sym as usize];
        wl.put(((e >> 5) as u64) << ns | signs as u64, (e & 31) + ns);
        for j in 0..4 {
            cur[line + j] = (sym >> (3 - j) & 1) as u16;
        }
        line += 4;
    }
    c.put_back(l);
    w.put_back(wl);
    commit(st, &prep, gr, ch, &cur);
}

/// The bits of `v` at the set bits of the 4-bit mask `m`, packed (highest first).
#[inline(always)]
fn pext4(v: u32, m: u32) -> u32 {
    PEXT4[(m << 4 | (v & 15)) as usize] as u32
}

/// Inverse of [`pext4`]: spreads the low bits of `v` over the set bits of `m`.
#[inline(always)]
fn pdep4(v: u32, m: u32) -> u32 {
    PDEP4[(m << 4 | (v & 15)) as usize] as u32
}

static PEXT4: [u8; 256] = {
    let mut t = [0u8; 256];
    let mut i = 0;
    while i < 256 {
        let (m, v) = (i >> 4, i & 15);
        let mut out = 0;
        let mut b = 4;
        while b > 0 {
            b -= 1;
            if m >> b & 1 != 0 {
                out = out << 1 | (v >> b & 1);
            }
        }
        t[i] = out as u8;
        i += 1;
    }
    t
};

static PDEP4: [u8; 256] = {
    let mut t = [0u8; 256];
    let mut i = 0;
    while i < 256 {
        let (m, v) = (i >> 4, i & 15);
        let mut out = 0;
        let mut k = 0;
        let mut b = 0;
        while b < 4 {
            if m >> b & 1 != 0 {
                out |= (v >> k & 1) << b;
                k += 1;
            }
            b += 1;
        }
        t[i] = out as u8;
        i += 1;
    }
    t
};

/// Compressing only: decodes part 3 of a granule from `r` (up to bit `end`) and codes it as
/// [`code_granule`] would, in one pass. Returns None, with the state untouched, if the
/// granule cannot be decoded; the caller then discards what was coded for it.
#[inline(never)]
#[allow(clippy::too_many_arguments)] // the hot path, kept flat
pub fn encode_granule<C: Coder>(
    c: &mut C,
    st: &mut State,
    h: Header,
    si: &SideInfo,
    gr: usize,
    ch: usize,
    scf: &[u8],
    r: &mut crate::bits::BitReader,
    end: usize,
) -> Option<huff::Part3> {
    use crate::huff::{BitBuf, FL, decoders};
    let g = &si.gr[gr][ch];
    if g.big_values > 288 {
        return None;
    }
    c.reserve(MAX_GRANULE_SYMS, MAX_GRANULE_RAW_BITS);
    let mut l = c.local();
    let d = decoders();
    let prep = prepare(st, h, si, gr, ch, scf);
    let mut cur = [0u16; 576];
    let reg = huff::regions(h, g);
    let mut line = 0usize;
    for t in 0..3 {
        let n = reg[t];
        let ts = g.table_select[t] as usize;
        if ts == 0 {
            line += 2 * n;
            continue;
        }
        let dec = d.pair[ts].as_deref()?;
        let fast = &d.fast[ts][..];
        let dim = DIM[ts] as u32;
        let lin = LINBITS[ts] as u32;
        let base = PAIR_BASE + ts * PAIR_PER_TABLE;
        let mut bb = BitBuf::new(r);
        if lin == 0 {
            for _ in 0..n {
                bb.refill();
                let w = bb.acc;
                let ctx =
                    base + (prep.bucket[line / 2] as usize * NPOS + pos_class(line)) * NCUR + cur_class(&cur, line);
                let e = fast[(w >> (64 - FL)) as usize] as u32;
                let (x, y, len, signs) = if e & 0x8000 != 0 {
                    let (x, y) = (e >> 6 & 15, e >> 2 & 15);
                    (x, y, e >> 10 & 31, if y != 0 { e & 3 } else { e >> 1 & 1 })
                } else {
                    let (len, sym) = dec.decode(w);
                    let (x, y) = (sym >> 4, sym & 15);
                    let ns = (x != 0) as u32 + (y != 0) as u32;
                    (x, y, len + ns, ((w << len) >> 1 >> (63 - ns)) as u32)
                };
                l.sym(ctx, x * dim + y);
                l.raw((x != 0) as u32 + (y != 0) as u32, signs);
                bb.consume(len);
                cur[line] = x as u16;
                cur[line + 1] = y as u16;
                line += 2;
            }
        } else {
            for _ in 0..n {
                bb.refill();
                let w = bb.acc;
                let bv = prep.bucket[line / 2] as usize;
                let ctx = base + (bv * NPOS + pos_class(line)) * NCUR + cur_class(&cur, line);
                let e = fast[(w >> (64 - FL)) as usize] as u32;
                let (len, sym) = if e & 0x8000 != 0 { (e >> 10 & 31, e >> 2 & 0xff) } else { dec.decode(w) };
                let (x, y) = (sym >> 4, sym & 15);
                l.sym(ctx, x * dim + y);
                let lctx = LIN_BASE + (ts - 16) * 16 + (bv / NOC).min(15);
                let mut rest = w << len;
                let mut used = len;
                let mut signs = 0;
                for (j, v) in [x, y].into_iter().enumerate() {
                    let mut m = v;
                    if v == 15 {
                        let ext = (rest >> (64 - lin)) as u32;
                        rest <<= lin;
                        used += lin;
                        let bl = 32 - ext.leading_zeros();
                        let top = if bl >= 2 { ext >> (bl - 2) & 1 } else { 0 };
                        l.sym(lctx, bl << 1 | top);
                        if bl > 2 {
                            l.raw(bl - 2, ext);
                        }
                        m = 15 + ext;
                    }
                    let s = (v != 0) as u32;
                    signs = signs << s | (rest >> 63) as u32 & s;
                    rest <<= s;
                    used += s;
                    cur[line + j] = m.min(u16::MAX as u32) as u16;
                }
                l.raw((x != 0) as u32 + (y != 0) as u32, signs);
                bb.consume(used);
                line += 2;
            }
        }
        r.pos = bb.pos();
        if r.pos > end {
            return None;
        }
    }
    let qbase = QUAD_BASE + if g.count1table { QUAD_PER_TABLE } else { 0 };
    let p = &st.prev[ch];
    let mut nq = 0;
    let mut bb = BitBuf::new(r);
    while line + 4 <= 576 {
        bb.refill();
        let (len, sym) = if g.count1table {
            (4, 15 - (bb.acc >> 60) as u32)
        } else {
            let e = d.count1a[(bb.acc >> 58) as usize];
            ((e >> 8) as u32, (e & 0xff) as u32)
        };
        let ns = sym.count_ones();
        if bb.pos() + (len + ns) as usize > end {
            break;
        }
        let pg = if prep.use_prev {
            (p[line + 1] != 0) as usize
                + (p[line + 2] != 0) as usize
                + (p[line + 3] != 0) as usize
                + (p[line + 4] != 0) as usize
        } else {
            5
        };
        l.sym(qbase + (pg * 3 + quad_other(&prep, st, line)) * NPOS + pos_class(line), sym);
        l.raw(ns, ((bb.acc << len) >> 1 >> (63 - ns)) as u32);
        bb.consume(len + ns);
        for j in 0..4 {
            cur[line + j] = (sym >> (3 - j) & 1) as u16;
        }
        line += 4;
        nq += 1;
    }
    r.pos = bb.pos();
    c.put_back(l);
    commit(st, &prep, gr, ch, &cur);
    Some(huff::Part3 { quads: nq, tail: end - r.pos })
}

/// A coder that only counts symbols per context (for training).
pub struct Counter {
    pub counts: Vec<Vec<u64>>,
}

impl Default for Counter {
    fn default() -> Self {
        Self::new()
    }
}

impl Counter {
    pub fn new() -> Counter {
        Counter { counts: (0..N_CTX).map(|c| vec![0; alphabet(c)]).collect() }
    }
}

impl Coder for Counter {
    type L = CounterLocal;
    fn local(&mut self) -> CounterLocal {
        CounterLocal(self.counts.as_mut_ptr())
    }
    fn put_back(&mut self, _l: CounterLocal) {}
}

pub struct CounterLocal(*mut Vec<u64>);

impl Sym for CounterLocal {
    fn sym(&mut self, ctx: usize, s: u32) -> u32 {
        assert!(ctx < N_CTX);
        // SAFETY: ctx < N_CTX, the length of the counts; valid while the counter lives.
        unsafe { (&mut *self.0.add(ctx))[s as usize] += 1 };
        s
    }
    fn raw(&mut self, _n: u32, v: u32) -> u32 {
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn step_factors() {
        assert_eq!(step_factor(0), 256);
        assert_eq!(step_factor(16), 32); // 2^-3
        assert_eq!(step_factor(-16), 2048);
        assert_eq!(step_factor(-80), 256 << 15);
    }
}
