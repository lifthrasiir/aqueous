//! The coefficient model. One code path serves both directions: `c.sym(ctx, s)` codes `s`
//! when compressing and returns the decoded symbol when decompressing.

use crate::frame::{Header, SideInfo, full_scalefactors, scf_layout, step_exponents};
use crate::huff;
use crate::huff_tables::{DIM, LINBITS};

pub trait Coder {
    /// Codes symbol `s` in context `ctx` (compressing), or decodes one (decompressing).
    fn sym(&mut self, ctx: usize, s: u32) -> u32;
    /// Codes the low `n` bits of `v` verbatim (n <= 32).
    fn raw(&mut self, n: u32, v: u32) -> u32;
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
}

/// At most this many symbols and raw bits per granule.
pub const MAX_GRANULE_SYMS: usize = 3 * 288 + 144;
pub const MAX_GRANULE_RAW_BITS: usize = 288 * 2 * 13 + 576;

/// Magnitude buckets of the pair context, plus one for "no neighbourhood".
pub const NB: usize = 24;
pub const NBX: usize = NB + 1;
/// Position classes of the pair context (64 lines each, the last one open).
pub const NPOS: usize = 4;
/// Pair contexts: (table_select, magnitude bucket, position class). The magnitude is the
/// neighbourhood in the previous granule (scaled to the current step sizes) and in the
/// other channel, so the contexts of a granule depend on nothing decoded in it.
pub const PAIR_BASE: usize = 0;
pub const PAIR_PER_TABLE: usize = NBX * NPOS;
pub const LIN_BASE: usize = PAIR_BASE + 32 * PAIR_PER_TABLE;
/// Escape bit length contexts: (table_select - 16, magnitude bucket capped at 15).
pub const LIN_ALPHA: usize = 14;
pub const QUAD_BASE: usize = LIN_BASE + 16 * 16;
/// Quadruple contexts: (count1 table, nonzero values at the same lines of the previous
/// granule 0..=4 or 5 if none, position class).
pub const QUAD_PER_TABLE: usize = 6 * NPOS;
pub const N_CTX: usize = QUAD_BASE + 2 * QUAD_PER_TABLE;

/// Alphabet size of each context.
pub fn alphabet(ctx: usize) -> usize {
    if ctx < LIN_BASE {
        let ts = (ctx - PAIR_BASE) / PAIR_PER_TABLE;
        let d = DIM[ts] as usize;
        (d * d).max(1)
    } else if ctx < QUAD_BASE {
        LIN_ALPHA
    } else {
        16
    }
}

/// About two buckets per octave.
#[inline(always)]
fn lb_calc(v: u32) -> u32 {
    if v < 4 {
        return v;
    }
    let l = 31 - v.leading_zeros();
    (2 * l + ((v >> (l - 1)) & 1)).min(NB as u32 - 1)
}

const LB_N: usize = 4096;
static LB: [u8; LB_N] = {
    let mut t = [0u8; LB_N];
    let mut v = 0;
    while v < LB_N {
        t[v] = if v < 4 {
            v as u8
        } else {
            let l = 31 - (v as u32).leading_zeros();
            let b = 2 * l + ((v as u32 >> (l - 1)) & 1);
            (if b > NB as u32 - 1 { NB as u32 - 1 } else { b }) as u8
        };
        v += 1;
    }
    t
};

#[inline(always)]
fn lb(v: u32) -> u32 {
    if (v as usize) < LB_N { LB[v as usize] as u32 } else { lb_calc(v) }
}

/// 2^(r/16) in 1/65536 units.
const POW16: [u32; 16] =
    [65536, 68438, 71468, 74632, 77936, 81386, 84990, 88752, 92682, 96785, 101070, 105545, 110218, 115098, 120194, 125515];

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
    exp: [[i16; 576]; 2],
    /// Magnitudes of the previous granule per channel, from index 1 (index 0 and the
    /// end are zero padding).
    prev: [[u16; 580]; 2],
    prev_ok: [bool; 2],
    /// Magnitudes of channel 0 of the current granule.
    cur0: [u16; 576],
}

impl State {
    pub fn new() -> Box<State> {
        Box::new(State { gr0_scf: [[0; 39]; 2], exp: [[0; 576]; 2], prev: [[0; 580]; 2], prev_ok: [false; 2], cur0: [0; 576] })
    }
}

#[inline(always)]
fn pos_class(line: usize) -> usize {
    (line >> 6).min(NPOS - 1)
}

/// Contexts of a granule, computed before coding it; the state is updated by [`commit`].
struct Prep {
    /// Magnitude bucket per pair (NB: none).
    bucket: [u8; 288],
    exp: [i16; 576],
    full: [u8; 39],
    use_prev: bool,
    short: bool,
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
    let mut exp = [0i16; 576];
    step_exponents(h, g, &full, &mut exp);
    if use_prev {
        let p = &st.prev[ch];
        let pe = &st.exp[ch];
        let mut i = 0;
        while i < npairs {
            // Step exponents are constant within a band: one factor per run.
            let (e0, pe0) = (exp[2 * i], pe[2 * i]);
            let f = step_factor((e0 - pe0) as i32);
            while i < npairs && exp[2 * i] == e0 && pe[2 * i] == pe0 {
                let k = 2 * i;
                let (pm, p0, p1, p2) = (p[k] as u64, p[k + 1] as u64, p[k + 2] as u64, p[k + 3] as u64);
                nbh[i] = (((2 * (p0 + p1) + pm + p2) * f) >> 8).min(1 << 24) as u32;
                i += 1;
            }
        }
    }
    if other {
        for i in 0..npairs {
            nbh[i] += 3 * (st.cur0[2 * i] as u32 + st.cur0[2 * i + 1] as u32);
        }
    }
    let w = if use_prev { 6 } else { 0 } + if other { 6 } else { 0 };
    let mut bucket = [NB as u8; 288];
    if w > 0 {
        let recip = ((16u64 << 16) + w as u64 - 1) / w as u64;
        for i in 0..npairs {
            bucket[i] = lb(((nbh[i] as u64 * recip) >> 16) as u32) as u8;
        }
    }
    Prep { bucket, exp, full, use_prev, short }
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
            let ctx = base + bv * NPOS + pos_class(line);
            let (a, b) = (coefs[line], coefs[line + 1]);
            let (x, y) = (a.unsigned_abs().min(15) as usize, b.unsigned_abs().min(15) as usize);
            let sym = c.sym(ctx, (x * dim + y) as u32);
            let xy = xy[sym as usize & 255] as u32;
            let (mut x, mut y) = (xy >> 4, xy & 15);
            if lin > 0 && (x == 15 || y == 15) {
                let lctx = LIN_BASE + (ts - 16) * 16 + bv.min(15);
                for (m, v0) in [(&mut x, a), (&mut y, b)] {
                    if *m == 15 {
                        let e = v0.unsigned_abs().wrapping_sub(15);
                        let bl = c.sym(lctx, 32 - e.leading_zeros());
                        let e = if bl == 0 { 0 } else { ((1u32 << (bl - 1)) | c.raw(bl - 1, e)) & ((1 << lin) - 1) };
                        *m = 15 + e;
                    }
                }
            }
            // Signs of the nonzero values, in one go.
            let nz = (x != 0) as u32 + (y != 0) as u32;
            let sbits = (((a < 0) as u32) << (y != 0) as u32 & ((x != 0) as u32) << (y != 0) as u32) | (y != 0 && b < 0) as u32;
            let sbits = c.raw(nz, sbits);
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
            (p[line + 1] != 0) as usize + (p[line + 2] != 0) as usize + (p[line + 3] != 0) as usize + (p[line + 4] != 0) as usize
        } else {
            5
        };
        let q = &coefs[line..line + 4];
        let sym = (q[0] != 0) as u32 * 8 + (q[1] != 0) as u32 * 4 + (q[2] != 0) as u32 * 2 + (q[3] != 0) as u32;
        let neg = (q[0] < 0) as u32 * 8 + (q[1] < 0) as u32 * 4 + (q[2] < 0) as u32 * 2 + (q[3] < 0) as u32;
        let sym = c.sym(qbase + pg * NPOS + pos_class(line), sym) & 15;
        // Sign bits of the nonzero values, first value in the highest bit.
        let ns = sym.count_ones();
        let packed = c.raw(ns, pext4(neg, sym));
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
                let ctx = base + prep.bucket[line / 2] as usize * NPOS + pos_class(line);
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
                c.sym(ctx, x * dim + y);
                c.raw((x != 0) as u32 + (y != 0) as u32, signs);
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
                let ctx = base + bv * NPOS + pos_class(line);
                let e = fast[(w >> (64 - FL)) as usize] as u32;
                let (len, sym) = if e & 0x8000 != 0 { (e >> 10 & 31, e >> 2 & 0xff) } else { dec.decode(w) };
                let (x, y) = (sym >> 4, sym & 15);
                c.sym(ctx, x * dim + y);
                let lctx = LIN_BASE + (ts - 16) * 16 + bv.min(15);
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
                        c.sym(lctx, bl);
                        if bl > 1 {
                            c.raw(bl - 1, ext);
                        }
                        m = 15 + ext;
                    }
                    let s = (v != 0) as u32;
                    signs = signs << s | (rest >> 63) as u32 & s;
                    rest <<= s;
                    used += s;
                    cur[line + j] = m.min(u16::MAX as u32) as u16;
                }
                c.raw((x != 0) as u32 + (y != 0) as u32, signs);
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
            (p[line + 1] != 0) as usize + (p[line + 2] != 0) as usize + (p[line + 3] != 0) as usize + (p[line + 4] != 0) as usize
        } else {
            5
        };
        c.sym(qbase + pg * NPOS + pos_class(line), sym);
        c.raw(ns, ((bb.acc << len) >> 1 >> (63 - ns)) as u32);
        bb.consume(len + ns);
        for j in 0..4 {
            cur[line + j] = (sym >> (3 - j) & 1) as u16;
        }
        line += 4;
        nq += 1;
    }
    r.pos = bb.pos();
    commit(st, &prep, gr, ch, &cur);
    Some(huff::Part3 { quads: nq, tail: end - r.pos })
}

/// A coder that only counts symbols per context (for training).
pub struct Counter {
    pub counts: Vec<Vec<u64>>,
}

impl Counter {
    pub fn new() -> Counter {
        Counter { counts: (0..N_CTX).map(|c| vec![0; alphabet(c)]).collect() }
    }
}

impl Coder for Counter {
    fn sym(&mut self, ctx: usize, s: u32) -> u32 {
        self.counts[ctx][s as usize] += 1;
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
