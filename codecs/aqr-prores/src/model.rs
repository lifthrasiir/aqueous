//! The coefficients' model: adaptive binary arithmetic coding of each picture's planes, block by
//! block in raster order, with the blocks around and the same block of the previous frame as
//! context. One code path ([`Coder`]) serves the encoder and the decoder.
//!
//! A block: its DC (predicted by the previous frame's, or by the median of its neighbours'),
//! the number of its nonzero AC coefficients (whether it is 0, then six bits of it less 1), then in scan order until that many are found:
//! whether each is nonzero, its size (bit length), its sign and its top mantissa bits; the
//! other mantissa bits go raw. The size's context is the expected magnitude: the previous
//! frame's coefficient, or the neighbouring blocks' and the coefficients above and to the left
//! in the block itself.

use crate::plane::Plane;

/// An adaptive bit: the probability of a 1 (16 bits, high) and a count (low bits). After coding
/// a bit `b` it moves towards the bit by `RATE[count]`/65536 of the distance, to `t = 65504`
/// for a 1 and 32 for a 0 (so that it stays within 32 to 65504), and the count, if below 30,
/// grows by 1.
#[derive(Clone, Copy)]
pub struct Prob(u32);

const LIMIT: u32 = 30;
const RATE: [i32; 32] = {
    let mut r = [0; 32];
    let mut i = 0;
    while i < 32 {
        r[i] = 65536 * 2 / (2 * i as i32 + 3);
        i += 1;
    }
    r
};

impl Default for Prob {
    fn default() -> Self {
        Prob(1 << 31)
    }
}

impl Prob {
    /// The probability of a 1, in 65536ths.
    #[inline(always)]
    pub fn p1(self) -> u32 {
        self.0 >> 16
    }

    #[inline(always)]
    pub fn update(&mut self, bit: bool) {
        let (p, n) = ((self.0 >> 16) as i32, self.0 & 31);
        let t = if bit { 65504 } else { 32 };
        let p = p + (((t - p) * RATE[n as usize]) >> 16);
        self.0 = (p as u32) << 16 | (n + (n < LIMIT) as u32);
    }
}

/// Encoder or decoder: `bit` codes `b` (encoding) or returns the bit decoded (decoding).
pub trait Coder {
    fn bit(&mut self, p: &mut Prob, b: bool) -> bool;
    /// As `bit`, for bits hard to predict whose value is data rather than control (selects in
    /// place of branches: a mispredicted branch costs more than the selects' wait).
    fn bit_u(&mut self, p: &mut Prob, b: bool) -> bool {
        self.bit(p, b)
    }
    /// `n` (0..=32) raw bits of `v`, or as many decoded.
    fn raw(&mut self, v: u32, n: u32) -> u32;
    /// Whether this decodes (values given to `bit`/`raw` are then ignored).
    const DECODE: bool;
    /// The kind of what is coded next (one of [`kind`]), for statistics.
    #[inline(always)]
    fn kind(&mut self, _: usize) {}
}

/// Kinds of decisions (names in [`kind::NAMES`]).
pub mod kind {
    pub const ZERO: usize = 0;
    /// The size's unary code: its first, second and later decisions.
    pub const SIZE: usize = 1;
    pub const MANT: usize = 4;
    pub const SIGN: usize = 6;
    pub const NZ: usize = 7;
    pub const DC: usize = 8;
    pub const NAMES: [&str; 9] = ["zero", "size0", "size1", "size2+", "mant0", "mant1", "sign", "nz", "dc"];
}

/// ProRes's progressive scan: scan position -> raster position in the 8x8 block.
pub const SCAN: [usize; 64] = [
    0, 1, 8, 9, 2, 3, 10, 11, 16, 17, 24, 25, 18, 19, 26, 27, 4, 5, 12, 20, 13, 6, 7, 14, 21, 28, 29, 22, 15, 23, 30,
    31, 32, 33, 40, 48, 41, 34, 35, 42, 49, 56, 57, 50, 43, 36, 37, 44, 51, 58, 59, 52, 45, 38, 39, 46, 53, 60, 61, 54,
    47, 55, 62, 63,
];

/// For each scan position, the scan positions of the raster neighbours above and to the left
/// (64, always zero, when there is none or it comes later in the scan).
const FNBR: [[u8; 2]; 64] = {
    let mut inv = [0usize; 64];
    let mut k = 0;
    while k < 64 {
        inv[SCAN[k]] = k;
        k += 1;
    }
    let mut t = [[64u8; 2]; 64];
    let mut k = 0;
    while k < 64 {
        let r = SCAN[k];
        if r >= 8 && inv[r - 8] < k {
            t[k][0] = inv[r - 8] as u8;
        }
        if !r.is_multiple_of(8) && inv[r - 1] < k {
            t[k][1] = inv[r - 1] as u8;
        }
        k += 1;
    }
    t
};

/// Bands of scan positions: each of the first 15 its own, then fours.
const KB: usize = 27;
const KBAND: [u8; 64] = {
    let mut t = [0u8; 64];
    let mut k = 1;
    while k < 64 {
        t[k] = if k <= 15 { (k - 1) as u8 } else { (15 + (k - 16) / 4) as u8 };
        k += 1;
    }
    t
};
/// Coarser bands for the mantissa.
const MBAND: [u8; 64] = {
    let mut t = [0u8; 64];
    let mut k = 1;
    while k < 64 {
        let b = [0u8, 1, 2, 3, 3, 4, 4, 4, 5, 5, 5, 5, 6, 6, 6, 6][(k - 1) / 4];
        t[k] = b;
        k += 1;
    }
    t
};

const NREM: usize = 11;
const REM_CLASS: [u8; 64] = {
    let mut t = [0u8; 64];
    let mut r = 1;
    while r < 64 {
        t[r] = match r {
            1..=4 => (r - 1) as u8,
            5..=6 => 4,
            7..=8 => 5,
            9..=12 => 6,
            13..=16 => 7,
            17..=24 => 8,
            25..=32 => 9,
            _ => 10,
        };
        r += 1;
    }
    t
};

const NB: usize = 16;
const PV: usize = 4;
/// Size steps after the first (sizes 1..=16).
const NS: usize = 15;
const CAT: usize = 17;

const NZB: usize = 16;
const NZ_BUCKET: [u8; 64] = {
    let mut t = [0u8; 64];
    let mut n = 0;
    while n < 64 {
        t[n] = match n {
            0..=7 => n as u8,
            8..=11 => 8,
            12..=15 => 9,
            16..=23 => 10,
            24..=31 => 11,
            32..=39 => 12,
            40..=47 => 13,
            48..=55 => 14,
            _ => 15,
        };
        n += 1;
    }
    t
};

/// What the visit of scan position `k` needs, in one place.
#[derive(Clone, Copy)]
struct KInfo {
    /// The scan positions of the neighbours above and to the left (64 for none).
    f: [u8; 2],
    /// The band for the signs, and for the mantissa.
    kb: u8,
    mb: u8,
    /// The offset of the band in the zero flags' contexts.
    z: u16,
    /// The same in the size's.
    s: u16,
}
const KINFO: [KInfo; 64] = {
    let mut t = [KInfo { f: [64, 64], kb: 0, mb: 0, z: 0, s: 0 }; 64];
    let mut k = 1;
    while k < 64 {
        t[k] = KInfo {
            f: FNBR[k],
            kb: KBAND[k],
            mb: MBAND[k],
            z: (KBAND[k] as usize * NREM * NB * 2 * PV) as u16,
            s: (KBAND[k] as usize * NB * 2 * PV * NS) as u16,
        };
        k += 1;
    }
    t
};

/// `REM_CLASS[r]` as an offset in the zero flags' contexts.
const REM_OFF: [u16; 64] = {
    let mut t = [0u16; 64];
    let mut r = 0;
    while r < 64 {
        t[r] = (REM_CLASS[r] as usize * NB * 2 * PV) as u16;
        r += 1;
    }
    t
};

/// For an expected magnitude `e` (capped at 255), `half_oct(e)` capped at `NB - 1`, as a
/// context offset.
const NBP: [u8; 256] = {
    let mut t = [0u8; 256];
    let mut e = 0;
    while e < 256 {
        let h = if e < 4 {
            e
        } else {
            let n = 31 - (e as u32).leading_zeros() as usize;
            2 * n + ((e >> (n - 1)) & 1)
        };
        t[e] = ((if h > NB - 1 { NB - 1 } else { h }) * 2 * PV) as u8;
        e += 1;
    }
    t
};
/// `min(cat(f), PV - 1)` for `f` capped at 4.
const PVT: [u8; 5] = [0, 1, 2, 2, 3];

#[inline(always)]
fn cat(v: i32) -> usize {
    (32 - v.unsigned_abs().leading_zeros()) as usize
}

/// The adaptive bits of the model.
pub struct Model {
    zero: Vec<Prob>,
    size: Vec<Prob>,
    mant: Vec<Prob>,
    sign: Vec<Prob>,
    nz: Vec<Prob>,
    dc: Vec<Prob>,
    dc_sign: Vec<Prob>,
    /// Scratch: a plane's blocks' nonzero counts and DC residual sizes.
    nzs: Vec<u8>,
    res: Vec<u8>,
}

impl Default for Model {
    fn default() -> Self {
        Self::new()
    }
}

impl Model {
    /// Bytes of a model's adaptive bits.
    pub fn bytes() -> u64 {
        let m = Model::new();
        let n = m.zero.len() + m.size.len() + m.mant.len() + m.sign.len() + m.nz.len() + m.dc.len() + m.dc_sign.len();
        (n * size_of::<Prob>()) as u64
    }

    /// A model of room for planes of `n` blocks in its scratch.
    pub fn with_scratch(n: usize) -> Self {
        let mut m = Model::new();
        m.nzs.reserve_exact(n);
        m.res.reserve_exact(n);
        m
    }

    /// Bytes of [`aqr_api::stack`] that [`Model::with_scratch`] takes.
    pub fn plan(n: usize) -> u64 {
        use aqr_api::stack::block;
        let m = Model::new();
        let bits = [m.zero.len(), m.size.len(), m.mant.len(), m.sign.len(), m.nz.len(), m.dc.len(), m.dc_sign.len()];
        bits.iter().map(|&k| block((k * size_of::<Prob>()) as u64)).sum::<u64>() + 2 * block(n as u64)
    }

    pub fn new() -> Self {
        let b = Prob::default();
        Model {
            zero: vec![b; 2 * KB * NREM * NB * 2 * PV],
            size: vec![b; 2 * KB * NB * 2 * PV * NS],
            mant: vec![b; 2 * CAT * 2 * 8 * 3 * 3],
            sign: vec![b; 2 * KB * 9],
            nz: vec![b; 2 * NZB * 9 * 64],
            dc: vec![b; 2 * 2 * 16 * (CAT + 1)],
            dc_sign: vec![b; 2 * 2 * 3],
            nzs: Vec::new(),
            res: Vec::new(),
        }
    }

    /// Codes plane `p` (decoding: fills it; its blocks must be zero) of component class `cc`
    /// (0 luma, 1 chroma), with `prev` the same plane of the previous frame if any.
    pub fn plane<C: Coder>(&mut self, c: &mut C, p: &mut Plane, prev: Option<&Plane>, cc: usize) {
        let prev = prev.filter(|q| q.bw == p.bw && q.bh == p.bh);
        let (bw, bh) = (p.bw, p.bh);
        self.nzs.clear();
        self.nzs.resize(bw * bh, 0);
        self.res.clear();
        self.res.resize(bw * bh, 0);
        let hp = prev.is_some() as usize;
        for by in 0..bh {
            for bx in 0..bw {
                let i = by * bw + bx;
                let q = prev.map(|q| &q.blocks[i]);
                self.dc(c, p, q, i, bx, by, cc);
                let n = self.nonzero(c, p, prev, i, bx, by, cc);
                if n > 0 {
                    self.ac(c, p, q, i, bx, by, cc, hp, n);
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    #[inline(always)]
    fn dc<C: Coder>(
        &mut self,
        c: &mut C,
        p: &mut Plane,
        q: Option<&[i16; 64]>,
        i: usize,
        bx: usize,
        by: usize,
        cc: usize,
    ) {
        let bw = p.bw;
        let pred = match q {
            Some(q) => q[0] as i32,
            None => match (bx > 0, by > 0) {
                (true, true) => {
                    let (l, a, al) =
                        (p.blocks[i - 1][0] as i32, p.blocks[i - bw][0] as i32, p.blocks[i - bw - 1][0] as i32);
                    l.max(a).min(l + a - al).max(l.min(a))
                }
                (true, false) => p.blocks[i - 1][0] as i32,
                (false, true) => p.blocks[i - bw][0] as i32,
                _ => 0,
            },
        };
        let rl = if bx > 0 { Some(self.res[i - 1] as usize) } else { None };
        let ra = if by > 0 { Some(self.res[i - bw] as usize) } else { None };
        let rb = match (rl, ra) {
            (Some(x), Some(y)) => (x + y).div_ceil(2),
            (Some(x), None) | (None, Some(x)) => x,
            _ => 0,
        };
        let hp = q.is_some() as usize;
        let ctx = ((cc * 2 + hp) * 16 + rb.min(15)) * (CAT + 1);
        let r = p.blocks[i][0] as i32 - pred;
        let k = self.unary(c, ctx, cat(r), CAT, (kind::DC, 0), |m| &mut m.dc);
        let r = if k == 0 {
            0
        } else {
            let neg = c.bit(&mut self.dc_sign[(cc * 2 + hp) * 3], r < 0);
            let m = (1u32 << (k - 1)) | c.raw(r.unsigned_abs() & ((1 << (k - 1)) - 1), k as u32 - 1);
            if neg { -(m as i32) } else { m as i32 }
        };
        self.res[i] = k as u8;
        if C::DECODE {
            p.blocks[i][0] = (pred + r) as i16;
        }
    }

    /// Codes `v` (0..max) as a unary code on the bits `bits(self)[ctx..]`.
    #[inline(always)]
    fn unary<C: Coder>(
        &mut self,
        c: &mut C,
        ctx: usize,
        v: usize,
        max: usize,
        (kind, spread): (usize, usize),
        bits: impl Fn(&mut Self) -> &mut Vec<Prob>,
    ) -> usize {
        let t = bits(self);
        let mut k = 0;
        loop {
            if k + 1 >= max {
                break;
            }
            c.kind(kind + k.min(spread));
            if !c.bit(&mut t[ctx + k], v > k) {
                break;
            }
            k += 1;
        }
        k
    }

    #[allow(clippy::too_many_arguments)]
    #[inline(always)]
    fn nonzero<C: Coder>(
        &mut self,
        c: &mut C,
        p: &Plane,
        prev: Option<&Plane>,
        i: usize,
        bx: usize,
        by: usize,
        cc: usize,
    ) -> usize {
        let bw = p.bw;
        let nl = if bx > 0 { Some(self.nzs[i - 1] as usize) } else { None };
        let na = if by > 0 { Some(self.nzs[i - bw] as usize) } else { None };
        let avg = match (nl, na) {
            (Some(x), Some(y)) => (x + y).div_ceil(2),
            (Some(x), None) | (None, Some(x)) => x,
            _ => 0,
        };
        let t = match prev {
            Some(q) => 1 + NZ_BUCKET[q.blocks[i][1..].iter().filter(|&&v| v != 0).count()] as usize / 2,
            None => 0,
        };
        let ctx = ((cc * NZB + NZ_BUCKET[avg] as usize) * 9 + t) * 64;
        let n = if C::DECODE { 0 } else { p.blocks[i][1..].iter().filter(|&&v| v != 0).count() };
        c.kind(kind::NZ);
        // Whether the count is 0 (the adaptive bit `nz[ctx]`), then if not, six bits of the
        // count less 1, top first, each in the context of those above it.
        let n = if !c.bit(&mut self.nz[ctx], n != 0) {
            0
        } else {
            let m = n.wrapping_sub(1);
            let mut node = 1;
            for j in (0..6).rev() {
                let b = c.bit(&mut self.nz[ctx + node], (m >> j) & 1 != 0);
                node = node * 2 + b as usize;
            }
            (node - 63).min(63)
        };
        self.nzs[i] = n as u8;
        n
    }

    #[allow(clippy::too_many_arguments)]
    #[inline(never)]
    fn ac<C: Coder>(
        &mut self,
        c: &mut C,
        p: &mut Plane,
        q: Option<&[i16; 64]>,
        i: usize,
        bx: usize,
        by: usize,
        cc: usize,
        hp: usize,
        n: usize,
    ) {
        let bw = p.bw;
        const ZERO: [i16; 64] = [0; 64];
        let tqs: &[i16; 64] = q.unwrap_or(&ZERO);
        // The expected magnitude before the in-block neighbours: twice the previous frame's,
        // or weighted neighbouring blocks (missing ones' weight to the others).
        let mut base = [0u32; 64];
        let wf = if q.is_some() {
            for k in 1..64 {
                base[k] = 2 * (tqs[k] as i32).unsigned_abs();
            }
            0
        } else {
            let has = |c: bool, j: usize| if c { &p.blocks[j] } else { &ZERO };
            let (ha, hl, hal, har) = (by > 0, bx > 0, bx > 0 && by > 0, bx + 1 < bw && by > 0);
            let (a, l) = (has(ha, i.wrapping_sub(bw)), has(hl, i.wrapping_sub(1)));
            let (al, ar) = (has(hal, i.wrapping_sub(bw + 1)), has(har, i.wrapping_sub(bw).wrapping_add(1)));
            // 2 * (near + far): near 2 (|a| + |l|), far |al| + |ar|; a missing one's share
            // to its pair, a missing pair's far half of near.
            let (wa, wl) = match (ha, hl) {
                (true, true) => (2, 2),
                (true, false) => (4, 0),
                (false, true) => (0, 4),
                _ => (0, 0),
            };
            let (wal, war, wa, wl) = match (hal, har) {
                (true, true) => (1, 1, wa, wl),
                (true, false) => (2, 0, wa, wl),
                (false, true) => (0, 2, wa, wl),
                _ => (0, 0, wa + wa / 2, wl + wl / 2),
            };
            for k in 1..64 {
                let m = |x: &[i16; 64]| (x[k] as i32).unsigned_abs();
                base[k] = 2 * (wa * m(a) + wl * m(l) + wal * m(al) + war * m(ar));
            }
            3
        };
        // |coefficients| of this block so far, and a zero at 64 for missing neighbours.
        let mut own = [0u32; 65];
        let hpv = hp * PV;
        let zoff = (cc * KB * NREM * NB) * 2 * PV + hpv;
        let soff = (cc * KB * NB) * 2 * PV * NS;
        let mut blk = p.blocks[i];
        let mut rem = n;
        let mut remo = REM_OFF[rem] as usize;
        for k in 1..64 {
            let tq = tqs[k] as i32;
            let ki = KINFO[k];
            let f = own[ki.f[0] as usize] + own[ki.f[1] as usize];
            let e = (base[k] + wf * f).min(255);
            let nbp = NBP[e as usize] as usize + PVT[f.min(4) as usize] as usize;
            let zctx = zoff + ki.z as usize + remo + nbp;
            let v = blk[k] as i32;
            c.kind(kind::ZERO);
            if !c.bit(&mut self.zero[zctx], v != 0) {
                continue;
            }
            let sctx = soff + ki.s as usize + (nbp + hpv) * NS;
            let k1 = 1 + self.unary(c, sctx, cat(v).wrapping_sub(1), NS + 1, (kind::SIZE, 2), |m| &mut m.size);
            // Mantissa: the two bits below the leading one modelled, the rest raw.
            let mag = v.unsigned_abs();
            let tc = cat(tq);
            let mut m = 1u32;
            let mb = ki.mb as usize;
            for j in 0..(k1 - 1).min(2) {
                let sh = k1 - 2 - j;
                let tm = if tc == k1 { 1 + ((tq.unsigned_abs() >> sh) & 1) as usize } else { 0 };
                let pre = if j == 0 { 0 } else { 1 + (m & 1) as usize };
                let ctx = ((((cc * CAT + k1) * 2 + j) * 8 + mb) * 3 + pre) * 3 + tm;
                c.kind(kind::MANT + j);
                m = m * 2 + c.bit_u(&mut self.mant[ctx], (mag >> sh) & 1 != 0) as u32;
            }
            if k1 > 3 {
                let r = k1 as u32 - 3;
                m = (m << r) | c.raw(mag & ((1 << r) - 1), r);
            }
            let ts = if tq == 0 {
                0
            } else if tq < 0 {
                1
            } else {
                2
            };
            let tcm = if tq == 0 {
                0
            } else if tc == k1 {
                1
            } else {
                2
            };
            c.kind(kind::SIGN);
            // Signs are close to even but where the previous frame has the coefficient.
            let neg = if tq == 0 {
                c.raw((v < 0) as u32, 1) != 0
            } else {
                c.bit_u(&mut self.sign[(cc * KB + ki.kb as usize) * 9 + ts * 3 + tcm], v < 0)
            };
            own[k] = m;
            blk[k] = if neg { -(m as i32) } else { m as i32 } as i16;
            rem -= 1;
            if rem == 0 {
                break;
            }
            remo = REM_OFF[rem] as usize;
        }
        if C::DECODE {
            p.blocks[i] = blk;
        }
    }
}
