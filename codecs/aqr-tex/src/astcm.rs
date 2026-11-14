//! ASTC as a family of the walk ([`crate::texture`]; FORMAT.md, "`astc-recons`"): each block as
//! the block to its left or above, or its head (all but its weights) theirs or a block's before,
//! or new: its kind (normal, void-extent, error), of normal blocks its block mode, partitions,
//! endpoint modes, colour values (predicted from a neighbour's of the same configuration, or from
//! the pixels predicted), and the codes of its integer sequences' groups as choices among those
//! of their values; then its weights, each by where the pixels predicted at its grid point fall
//! between the endpoints there, and the weights around it in the grid. One plane of RGBA.

use aqr_common::arith::Bit;

use crate::astc::{
    self, Block, COLOUR_ORDERS, COLOUR_RANGES, Kind, QUINTS, Range, Sequence, TRITS, WEIGHT_ORDERS, WEIGHT_RANGES,
};
use crate::coder::{Cat, Side, bit_models, unzz, wide_token, zz};
use crate::copies::{BLOCK_LEFT, BLOCK_UP, Copies, Got, NEW, NONE};
use crate::layout::{Format, Group, Shape};
use crate::plane::{Plane, Up, med};
use crate::texture::Family;
use crate::{Result, corrupt};

pub struct Astc;

/// `v` modulo `levels` (as `rem_euclid`; of the ranks' sums and differences, by a test, where
/// they are within one turn).
#[inline(always)]
fn wrap(v: i32, levels: i32) -> i32 {
    match v {
        _ if (0..levels).contains(&v) => v,
        _ if (-levels..0).contains(&v) => v + levels,
        _ => v.rem_euclid(levels),
    }
}

/// Contexts of colour values: the source (3), the value's place in its partition's (8), the
/// range's class (5); of weights: the range (12), the class of the predictions' agreement (4),
/// the plane (2).
const COLOUR_CTX: usize = 3 * 8 * 5;
const WEIGHT_CTX: usize = 12 * 4 * 2;

bit_models! {
    /// The models of a texture, or of a stripe.
    #[derive(Clone)]
    pub struct Models {
        copies: Copies<16>,
        kind: 4 * 4 * 4,
        same: 8,
        mode: 2048,
        np: 5 * 4,
        seed: 1024,
        cem: 5 * 16 * 16 + 4 * 4 + 8 * 2 * 4,
        ccs: 4,
        colour: COLOUR_CTX * 16,
        colour_next: COLOUR_CTX * 16,
        choice: 2 * 6 * 2 * 8,
        unused: 8,
        weight: WEIGHT_CTX * 16,
        weight_next: WEIGHT_CTX * 16,
        void: 8 * 32 + 2,
        void_next: 8 * 32,
    }
}

/// What a block of a surface was, for the blocks after: its fields and its choice.
#[derive(Clone, Copy)]
struct Seen {
    b: Block,
    choice: u8,
}

/// Buffers of a surface's coding: its blocks seen, an upsampling's, and the nearest ranks.
pub struct Work {
    seen: Vec<Seen>,
    up: Up<u16>,
    ranks: Vec<u8>,
}

/// Bytes of the nearest ranks ([`near_ranks`]).
const RANKS: usize = COLOUR_ORDERS.len() * 256 + WEIGHT_ORDERS.len() * 65;

/// The nearest ranks ([`astc::Order::nearest`]) to each value: of each colour range, of 0 to
/// 255; then of each weight range, of 0 to 64.
fn near_ranks(out: &mut Vec<u8>) {
    out.clear();
    for o in &COLOUR_ORDERS {
        out.extend((0..256).map(|u| o.nearest(u) as u8));
    }
    for o in &WEIGHT_ORDERS {
        out.extend((0..65).map(|u| o.nearest(u) as u8));
    }
}

/// The nearest rank to `u` of colour range `range` (`weight` false) or of weight range `range`,
/// by [`near_ranks`] (beyond the values, the nearest to the end of them).
#[inline(always)]
fn nearest(ranks: &[u8], weight: bool, range: usize, u: i32) -> i32 {
    let at = match weight {
        false => range * 256 + u.clamp(0, 255) as usize,
        true => COLOUR_ORDERS.len() * 256 + range * 65 + u.clamp(0, 64) as usize,
    };
    ranks[at] as i32
}

#[derive(Default)]
pub struct Planes {
    p: Option<Plane<u8, 4>>,
}

/// The head of a block of footprint `fp`: of a normal block its bits below its weights (and a
/// mark), else all of them.
fn head(raw: &[u8; 16], fp: (u32, u32)) -> u128 {
    let v = u128::from_le_bytes(*raw);
    match astc::legal(raw, fp) {
        Some(bits) => v & ((1u128 << (128 - bits)) - 1) | 1 << 127,
        None => v,
    }
}

/// Codes the choice of each group of sequence `s` (when encoding, `want`'s codes) of range `r`,
/// among the codes of its values: an index where there are several.
fn choices(s: &mut impl Side, md: &mut Models, seq: &mut Sequence, want: &Sequence, r: Range, weight: bool) {
    if !r.trits && !r.quints {
        return;
    }
    let (codes, g, base) = if r.trits { (&TRITS, 5u32, 3u32) } else { (&QUINTS, 3, 5) };
    let n = seq.n as u32;
    for grp in 0..n.div_ceil(g) {
        let k = (n - grp * g).min(g);
        let tuple = (0..k).rev().fold(0, |t, i| t * base + (seq.v[(grp * g + i) as usize] as u32 >> r.bits));
        let of = codes.of(k as usize, tuple as usize);
        let ix = match of.len() {
            1 => 0,
            n => {
                let w = of.iter().position(|&c| c == want.codes[grp as usize]).unwrap_or(0);
                let bits = 32 - (n as u32 - 1).leading_zeros();
                let ctx = ((r.trits as usize * 6 + k as usize) * 2 + weight as usize) * 8;
                (s.tree(&mut md.choice[ctx..ctx + 8], bits, w as u32, Cat::Mode) as usize).min(n - 1)
            }
        };
        seq.codes[grp as usize] = of[ix];
    }
}

/// The predicted pixels around a block: of the next smaller mipmap at each texel, of the column to
/// its left and the row above (`n` of each).
struct Around<'a> {
    coarse: Option<&'a Plane<u8, 4>>,
    left: Option<[[i32; 4]; 12]>,
    top: Option<[[i32; 4]; 12]>,
    n: (usize, usize),
}

impl Around<'_> {
    fn near(&self) -> impl Iterator<Item = &[i32; 4]> {
        let l = self.left.iter().flat_map(|l| &l[..self.n.1]);
        l.chain(self.top.iter().flat_map(|t| &t[..self.n.0]))
    }
}

/// Block `n` where it is normal.
fn normal(n: Option<&Block>) -> Option<&Block> {
    n.filter(|n| n.kind == Kind::Normal)
}

/// Block `n` where it is normal of the partitions, endpoint modes and colour range of `b`.
fn alike<'a>(n: Option<&'a Block>, b: &Block) -> Option<&'a Block> {
    normal(n).filter(|n| n.np == b.np && n.cr == b.cr && n.cem[..b.np as usize] == b.cem[..b.np as usize])
}

impl Astc {
    /// The predicted values of each colour value of new block `b` (its endpoint modes and range
    /// known): from the left or upper block of the same partitions' modes and range (source 0),
    /// else from the pixels predicted of the modes of direct and scaled colours (1), else 128 (2).
    fn predicted(
        b: &Block,
        left: Option<&Block>,
        up: Option<&Block>,
        a: &Around,
        (bx, by, fp): (usize, usize, (u32, u32)),
    ) -> ([i32; 18], [u8; 18]) {
        let mut out = [128i32; 18];
        let mut src = [2u8; 18];
        let nv = b.colours.n as usize;
        if let Some(n) = alike(left, b).or(alike(up, b)) {
            let r = COLOUR_RANGES[b.cr as usize];
            for i in 0..nv {
                out[i] = astc::unquantize_colour(n.colours.v[i] as u32, r);
                src[i] = 0;
            }
            return (out, src);
        }
        let texels = fp.0 * fp.1;
        let parts = astc::Partitions::new(b.seed as u32, b.np as u32, texels);
        let mut of = [0u8; 144];
        if a.coarse.is_some() {
            for t in 0..texels {
                of[t as usize] = parts.of(t % fp.0, t / fp.0) as u8;
            }
        }
        let mut at = 0;
        for p in 0..b.np as usize {
            let cem = b.cem[p];
            let n = 2 * astc::pairs(cem) as usize;
            // the pixels of the partition, predicted (else those around)
            let (mut px, mut m) = ([[0i32; 4]; 144], 0);
            if let Some(c) = a.coarse {
                for t in 0..texels {
                    let (x, y) = (t % fp.0, t / fp.0);
                    if of[t as usize] as usize == p {
                        px[m] =
                            c.at(bx * fp.0 as usize + x as usize, by * fp.1 as usize + y as usize).map(|c| c as i32);
                        m += 1;
                    }
                }
            }
            if m == 0 {
                for v in a.near() {
                    px[m] = *v;
                    m += 1;
                }
            }
            if m > 0 {
                let px = &px[..m];
                let lum = |v: &[i32; 4]| v[0] + v[1] + v[2];
                let lo = *px.iter().min_by_key(|v| lum(v)).unwrap();
                let hi = *px.iter().rev().max_by_key(|v| lum(v)).unwrap();
                let l3 = |v: [i32; 4]| (v[0] + v[1] + v[2]) / 3;
                let s = (256 * lum(&lo) / lum(&hi).max(1)).clamp(0, 255);
                let v: &[i32] = match cem {
                    0 => &[l3(lo), l3(hi)],
                    4 => &[l3(lo), l3(hi), lo[3], hi[3]],
                    8 => &[lo[0], hi[0], lo[1], hi[1], lo[2], hi[2]],
                    12 => &[lo[0], hi[0], lo[1], hi[1], lo[2], hi[2], lo[3], hi[3]],
                    6 => &[hi[0], hi[1], hi[2], s],
                    10 => &[hi[0], hi[1], hi[2], s, lo[3], hi[3]],
                    _ => &[],
                };
                if v.len() == n {
                    for k in 0..n {
                        out[at + k] = v[k];
                        src[at + k] = 1;
                    }
                }
            }
            at += n;
        }
        (out, src)
    }

    /// The colour values of new block `b` (when encoding, `want`'s), by their predictions.
    #[allow(clippy::too_many_arguments)]
    fn colours(
        s: &mut impl Side,
        md: &mut Models,
        ranks: &[u8],
        b: &mut Block,
        want: &Block,
        left: Option<&Block>,
        up: Option<&Block>,
        a: &Around,
        at: (usize, usize, (u32, u32)),
    ) {
        let (pred, src) = Self::predicted(b, left, up, a, at);
        let r = COLOUR_RANGES[b.cr as usize];
        let ord = &COLOUR_ORDERS[b.cr as usize];
        let levels = r.levels() as i32;
        let class = (b.cr as usize / 4).min(4);
        let mut place = 0;
        let mut p = 0;
        for i in 0..b.colours.n as usize {
            let n = 2 * astc::pairs(b.cem[p]) as usize;
            let ctx = ((src[i] as usize * 8 + place.min(7)) * 5 + class) * 16;
            let pr = nearest(ranks, false, b.cr as usize, pred[i]);
            let wr = ord.rank[want.colours.v[i] as usize] as i32;
            let d = wrap(wr - pr, levels);
            let d = if d >= (levels + 1) / 2 { d - levels } else { d };
            let v = s.token(&mut md.colour[ctx..ctx + 16], &mut md.colour_next[ctx..ctx + 16], zz(d), Cat::Endpoints);
            let rank = wrap(pr + unzz(v), levels);
            b.colours.v[i] = ord.value[rank as usize];
            place += 1;
            if place == n {
                (place, p) = (0, p + 1);
            }
        }
        choices(s, md, &mut b.colours, &want.colours, r, false);
    }

    /// The weights of block `b` at (bx, by) (its colours known; when encoding, `want`'s), each by
    /// the pixel predicted at its grid point between the endpoints there and the weights around it.
    #[allow(clippy::too_many_arguments)]
    fn weights(
        s: &mut impl Side,
        md: &mut Models,
        ranks: &[u8],
        b: &mut Block,
        want: &Block,
        coarse: Option<&Plane<u8, 4>>,
        (bx, by, fp): (usize, usize, (u32, u32)),
    ) {
        let r = WEIGHT_RANGES[b.wr as usize];
        let ord = &WEIGHT_ORDERS[b.wr as usize];
        let levels = r.levels() as i32;
        let (wx, wy) = (b.wx as u32, b.wy as u32);
        let planes = 1 + b.dual as usize;
        let ends = astc::colour_ends(b);
        let texels = fp.0 * fp.1;
        let n = (wx * wy) as usize * planes;
        b.weights.n = n as u8;
        let unq = |b: &Block, i: usize| ord.unq[b.weights.v[i] as usize] as i32;
        let parts = astc::Partitions::new(b.seed as u32, b.np as u32, texels);
        // (of each partition and plane, its line between the endpoints: its start, its way of the
        // plane's channels and their square; of 8-bit endpoints, within i32 below)
        let mut line = [[([0i32; 4], [0i32; 4], 0i32); 2]; 4];
        for (p, l) in line.iter_mut().enumerate().take(b.np as usize) {
            for (plane, l) in l.iter_mut().enumerate().take(planes) {
                let (e0, e1) = (ends[p][0], ends[p][1]);
                let d: [i32; 4] = std::array::from_fn(|c| {
                    let ours = !b.dual || (c == b.ccs as usize) == (plane == 1);
                    if ours { e1[c].clamp(0, 255) - e0[c].clamp(0, 255) } else { 0 }
                });
                *l = (e0.map(|c| c.clamp(0, 255)), d, d.iter().map(|v| v * v).sum());
            }
        }
        // (the texel of each column and row of the grid)
        let place = |g: u32, w: u32, f: u32| if w > 1 { (g * (f - 1) + (w - 1) / 2) / (w - 1) } else { 0 };
        let txs: [u32; 12] = std::array::from_fn(|g| place(g as u32, wx, fp.0));
        let tys: [u32; 12] = std::array::from_fn(|g| place(g as u32, wy, fp.1));
        for i in 0..n {
            let (g, plane) = (i / planes, i % planes);
            let (gx, gy) = (g as u32 % wx, g as u32 / wx);
            // (the texel of the grid point, its partition, the pixel predicted there)
            let (tx, ty) = (txs[gx as usize % 12], tys[gy as usize % 12]);
            let p = parts.of(tx, ty) as usize;
            let (e0, d, l2) = line[p][plane];
            let wp = coarse.filter(|_| l2 > 0).map(|cp| {
                let q = cp.at(bx * fp.0 as usize + tx as usize, by * fp.1 as usize + ty as usize);
                let dot: i32 = (0..4).map(|c| (q[c] as i32 - e0[c]) * d[c]).sum();
                ((dot * 64 + l2 / 2).div_euclid(l2)).clamp(0, 64)
            });
            let near = |dx: u32, dy: u32| {
                (gx >= dx && gy >= dy).then(|| unq(b, ((gy - dy) * wx + gx - dx) as usize * planes + plane))
            };
            let (l, u, ul) = (near(1, 0), near(0, 1), near(1, 1));
            let m = match (l, u, ul) {
                (Some(l), Some(u), Some(ul)) => Some(med(l, u, ul)),
                (Some(v), _, _) | (None, Some(v), _) => Some(v),
                _ => None,
            };
            let (pred, agree) = match (wp, m) {
                (Some(w), Some(m)) => (w, ((w - m).abs() / 12).min(2) as usize),
                (Some(w), None) => (w, 2),
                (None, Some(m)) => (m, 3),
                (None, None) => (32, 3),
            };
            let ctx = ((b.wr as usize * 4 + agree) * 2 + plane) * 16;
            let pr = nearest(ranks, true, b.wr as usize, pred);
            let wr = ord.rank[want.weights.v[i] as usize] as i32;
            let d = wrap(wr - pr, levels);
            let d = if d >= (levels + 1) / 2 { d - levels } else { d };
            let v = s.token(&mut md.weight[ctx..ctx + 16], &mut md.weight_next[ctx..ctx + 16], zz(d), Cat::Indices);
            let rank = wrap(pr + unzz(v), levels);
            b.weights.v[i] = ord.value[rank as usize];
        }
        choices(s, md, &mut b.weights, &want.weights, r, true);
    }

    /// A new normal block's head (when encoding, `want`'s): its block mode, partitions, endpoint
    /// modes, dual planes' component, colour values and the bits no field holds.
    #[allow(clippy::too_many_arguments)]
    fn head(
        s: &mut impl Side,
        md: &mut Models,
        ranks: &[u8],
        want: &Block,
        left: Option<&Block>,
        up: Option<&Block>,
        a: &Around,
        at: (usize, usize, (u32, u32)),
    ) -> Result<Block> {
        let (ln, un) = (normal(left), normal(up));
        // the block mode: the left one's, the upper one's, or its bits
        let mode = match (ln, un) {
            (Some(l), _) if s.bit(&mut md.same[0], want.mode == l.mode, Cat::Mode) => l.mode,
            (_, Some(u))
                if Some(u.mode) != ln.map(|l| l.mode) && s.bit(&mut md.same[1], want.mode == u.mode, Cat::Mode) =>
            {
                u.mode
            }
            _ => s.tree(&mut md.mode, 11, want.mode as u32, Cat::Mode) as u16,
        };
        let (wx, wy, wr, dual) = astc::block_mode(mode as u32).ok_or_else(corrupt)?;
        let lnp = ln.map_or(4, |l| l.np as usize - 1);
        let np = s.tree(&mut md.np[lnp * 4..][..4], 2, want.np.wrapping_sub(1) as u32 & 3, Cat::Mode) as u8 + 1;
        let mut b = Block {
            kind: Kind::Normal,
            mode,
            wx: wx as u8,
            wy: wy as u8,
            wr: wr as u8,
            dual,
            np,
            ..Default::default()
        };
        if np > 1 {
            b.seed = match ln.filter(|l| l.np == np) {
                Some(l) if s.bit(&mut md.same[2], want.seed == l.seed, Cat::Mode) => l.seed,
                _ => s.tree(&mut md.seed, 10, want.seed as u32, Cat::Mode) as u16,
            };
            b.sel = s.tree(&mut md.cem[5 * 256..][..4], 2, want.sel as u32, Cat::Mode) as u8;
        }
        // the endpoint modes: of one partition, or of all alike, one; else each's class and mode
        let lc = ln.map_or(16, |l| l.cem[0] as usize);
        if np == 1 || b.sel == 0 {
            let ctx = (np as usize - 1).min(4) * 256 + lc * 16;
            let c = s.tree(&mut md.cem[ctx..ctx + 16], 4, want.cem[0] as u32, Cat::Mode) as u8;
            b.cem = [c; 4];
        } else {
            for p in 0..np as usize {
                let base = 5 * 256 + 4;
                let class = s.bit(
                    &mut md.cem[base + (b.sel as usize - 1) * 4 + p.min(3)],
                    want.cem[p] >> 2 != b.sel - 1,
                    Cat::Mode,
                ) as u8;
                let m = s.tree(
                    &mut md.cem[base + 16 + (class as usize * 4 + p.min(3)) * 4..][..4],
                    2,
                    (want.cem[p] & 3) as u32,
                    Cat::Mode,
                ) as u8;
                b.cem[p] = (b.sel - 1 + class) << 2 | m;
            }
        }
        if dual {
            b.ccs = s.tree(&mut md.ccs, 2, want.ccs as u32, Cat::Mode) as u8;
        }
        // the colour values' range, by the bits left them
        let nw = wx * wy * (1 + dual as u32);
        let wbits = WEIGHT_RANGES[wr].size(nw);
        let extra = if np > 1 && b.sel != 0 { 3 * np as u32 - 4 } else { 0 };
        let start = if np == 1 { 17 } else { 29 };
        let ccs_at = 128 - wbits as i32 - extra as i32 - 2 * dual as i32;
        let nv: u32 = (0..np as usize).map(|p| 2 * astc::pairs(b.cem[p])).sum();
        let avail = ccs_at - start as i32;
        if nv > 18 || nw > 64 || avail < (13 * nv).div_ceil(5) as i32 {
            return Err(corrupt());
        }
        let cr =
            (0..COLOUR_RANGES.len()).rev().find(|&i| COLOUR_RANGES[i].size(nv) as i32 <= avail).ok_or_else(corrupt)?;
        b.cr = cr as u8;
        b.colours.n = nv as u8;
        Self::colours(s, md, ranks, &mut b, want, left, up, a, at);
        // the bits no field holds, as they are
        let end = start + COLOUR_RANGES[cr].size(nv);
        (b.unused_at, b.unused_n) = (end as u8, (ccs_at - end as i32) as u8);
        for k in 0..b.unused_n as u32 {
            let v = s.bit(&mut md.unused[k.min(7) as usize], want.unused >> k & 1 != 0, Cat::Mode);
            b.unused |= (v as u128) << k;
        }
        Ok(b)
    }

    /// A new void-extent block (when encoding, `want`'s): its range bit, its extents (all 1s, or as
    /// they are), its colour by the mean of the pixels predicted (of LDR, as 16 bits).
    fn void(
        s: &mut impl Side,
        md: &mut Models,
        want: &Block,
        left: Option<&Block>,
        a: &Around,
        at: (usize, usize, (u32, u32)),
    ) -> Block {
        let w = u128::from_le_bytes(want.raw);
        let mut v: u128 = 0x1fc | 3 << 10;
        let hdr = s.bit(&mut md.void[8 * 32], w >> 9 & 1 != 0, Cat::Mode);
        v |= (hdr as u128) << 9;
        let ext = (w >> 12) & ((1u128 << 52) - 1);
        let ones = s.bit(&mut md.void[8 * 32 + 1], ext == (1u128 << 52) - 1, Cat::Mode);
        v |= match ones {
            true => ((1u128 << 52) - 1) << 12,
            false => {
                (0..52).fold(0u128, |e, k| e | (s.raw(1, (ext >> (51 - k)) as u32 & 1, Cat::Mode) as u128) << (51 - k))
                    << 12
            }
        };
        // (the mean of the pixels predicted; of HDR, the left block's colour)
        let (fw, fh) = at.2;
        let mut sum = [0i64; 4];
        let mut n = 0;
        if let Some(c) = a.coarse {
            for t in 0..fw * fh {
                let px = c.at(at.0 * fw as usize + (t % fw) as usize, at.1 * fh as usize + (t / fw) as usize);
                (0..4).for_each(|k| sum[k] += px[k] as i64);
                n += 1;
            }
        } else {
            for px in a.near() {
                (0..4).for_each(|k| sum[k] += px[k] as i64);
                n += 1;
            }
        }
        let lv = left.filter(|l| l.kind == Kind::Void).map(|l| u128::from_le_bytes(l.raw));
        for k in 0..4 {
            let pred = match (hdr, lv) {
                (_, Some(l)) => (l >> (64 + 16 * k)) as i32 & 0xffff,
                (false, None) if n > 0 => ((sum[k] + n / 2) / n) as i32 * 257,
                (false, None) => 0x8080,
                (true, None) => 0x3c00,
            };
            let ctx = (hdr as usize * 4 + k) * 32;
            let want_c = (w >> (64 + 16 * k)) as i32 & 0xffff;
            let r = ((want_c - pred) << 16) >> 16;
            let got =
                wide_token(s, &mut md.void[ctx..ctx + 32], &mut md.void_next[ctx..ctx + 32], zz(r), Cat::Endpoints);
            let c = (pred + unzz(got)) & 0xffff;
            v |= (c as u128) << (64 + 16 * k);
        }
        Block::read(&v.to_le_bytes(), at.2)
    }

    /// Codes the block at (`bx`, `by`) of a surface `bw` blocks wide (when encoding, `raw`).
    #[allow(clippy::too_many_arguments)]
    fn block(
        s: &mut impl Side,
        md: &mut Models,
        ranks: &[u8],
        raw: &[u8; 16],
        seen: &[Seen],
        (bx, by, bw, fp): (usize, usize, usize, (u32, u32)),
        plane: &mut Plane<u8, 4>,
        coarse: Option<&Plane<u8, 4>>,
    ) -> Result<Seen> {
        let i = by * bw + bx;
        let left = (bx > 0).then(|| &seen[i - 1]);
        let up = (by > 0).then(|| &seen[i - bw]);
        let lc = left.map_or(NONE, |l| l.choice);
        let uc = up.map_or(NONE, |u| u.choice);
        let want = if s.encoding() { Block::read(raw, fp) } else { Block::default() };
        let h = |b: &[u8; 16]| head(b, fp);
        let got = md.copies.code(s, raw, left.map(|l| &l.b.raw), up.map(|u| &u.b.raw), lc, uc, h)?;
        let (b, choice) = match got {
            Got::Block(c, choice) => {
                let b = match choice {
                    BLOCK_LEFT => left.unwrap().b,
                    BLOCK_UP => up.unwrap().b,
                    _ => Block::read(&c, fp),
                };
                (b, choice)
            }
            Got::Head(c, choice) => {
                let mut b = Block::read(&c, fp);
                if b.kind == Kind::Normal {
                    Self::weights(s, md, ranks, &mut b, &want, coarse, (bx, by, fp));
                    b.raw = b.write();
                }
                (b, choice)
            }
            Got::New => {
                let (l, u) = (left.map(|l| &l.b), up.map(|u| &u.b));
                // (kinds: void-extent 0, error 1, normal 2)
                let kc = |n: Option<&Block>| n.map_or(3, |n| n.kind as usize);
                let ctx = (kc(l) * 4 + kc(u)) * 4;
                let kind = s.tree(&mut md.kind[ctx..ctx + 4], 2, want.kind as u32, Cat::Mode);
                let px = |x: usize, y: usize| plane.at(x, y).map(|c| c as i32);
                let (fw, fh) = (fp.0 as usize, fp.1 as usize);
                let a = Around {
                    coarse,
                    left: (bx > 0)
                        .then(|| std::array::from_fn(|k| if k < fh { px(bx * fw - 1, by * fh + k) } else { [0; 4] })),
                    top: (by > 0)
                        .then(|| std::array::from_fn(|k| if k < fw { px(bx * fw + k, by * fh - 1) } else { [0; 4] })),
                    n: (fw, fh),
                };
                let b = match kind {
                    0 => {
                        let mut b = Self::void(s, md, &want, l, &a, (bx, by, fp));
                        b.raw = b.write();
                        b
                    }
                    1 => {
                        let mut r = [0u8; 16];
                        for k in 0..16 {
                            r[k] = s.raw(8, want.raw[k] as u32, Cat::Mode) as u8;
                        }
                        Block::read(&r, fp)
                    }
                    _ => {
                        let mut b = Self::head(s, md, ranks, &want, l, u, &a, (bx, by, fp))?;
                        Self::weights(s, md, ranks, &mut b, &want, coarse, (bx, by, fp));
                        b.raw = b.write();
                        // (a legal block's fields are what reading it gives; else it is read)
                        match astc::legal(&b.raw, fp) {
                            Some(_) => b,
                            None => Block::read(&b.raw, fp),
                        }
                    }
                };
                (b, NEW)
            }
        };
        let (fw, fh) = (fp.0 as usize, fp.1 as usize);
        // (a copy of the left or upper block: its pixels; else the block's decoded)
        let from = match choice {
            BLOCK_LEFT => Some((bx * fw - fw, by * fh)),
            BLOCK_UP => Some((bx * fw, by * fh - fh)),
            _ => None,
        };
        if let Some((x0, y0)) = from {
            for y in 0..fh {
                for x in 0..fw {
                    plane.set(bx * fw + x, by * fh + y, plane.at(x0 + x, y0 + y));
                }
            }
            return Ok(Seen { b, choice });
        }
        let mut px = [[0u8; 4]; 144];
        astc::pixels(&b, fp, &mut px);
        for t in 0..(fp.0 * fp.1) as usize {
            plane.set(bx * fp.0 as usize + t % fp.0 as usize, by * fp.1 as usize + t / fp.0 as usize, px[t]);
        }
        Ok(Seen { b, choice })
    }
}

impl Family for Astc {
    const GROUP: Group = Group::Astc;

    #[inline(always)]
    fn shape(f: Format) -> Shape {
        let (fw, fh) = f.footprint();
        Shape { fw, fh, least: 1, bytes: 16 }
    }

    type Models = Models;
    type Planes = Planes;
    type Bufs = Vec<[u8; 4]>;
    type Work = Work;

    fn models(_: Format) -> Models {
        Models::new()
    }

    fn models_plan(_: Format) -> u64 {
        Models::plan()
    }

    fn copy_models(m: &mut Models, o: &Models) {
        m.copy_from(o);
    }

    fn bufs(_: Format, px: usize) -> Vec<[u8; 4]> {
        Vec::with_capacity(px)
    }

    fn bufs_plan(_: Format, px: usize) -> u64 {
        aqr_api::stack::block(4 * px as u64)
    }

    fn give(b: &mut Vec<[u8; 4]>, p: Planes) {
        if let Some(p) = p.p {
            *b = p.into_px();
        }
    }

    fn is_empty(p: &Planes) -> bool {
        p.p.is_none()
    }

    fn work(_: Format, blocks: usize, cols: usize, across: usize) -> Work {
        let mut ranks = Vec::with_capacity(RANKS);
        near_ranks(&mut ranks);
        Work { seen: Vec::with_capacity(blocks), up: Up::with_capacity(cols, across), ranks }
    }

    fn work_plan(_: Format, blocks: usize, cols: usize, across: usize) -> u64 {
        use aqr_api::stack::block;
        block((blocks * size_of::<Seen>()) as u64) + Up::<u16>::plan(cols, across) + block(RANKS as u64)
    }

    fn across_values(_: Format) -> usize {
        4
    }

    fn planes_of(f: Format, b: &[u8], (bw, bh): (usize, usize), p: &mut Planes) {
        let fp = f.footprint();
        let fp = (fp.0 as u32, fp.1 as u32);
        let plane = p.p.as_mut().expect("a plane");
        let y0 = plane.grow(bh * fp.1 as usize);
        let mut px = [[0u8; 4]; 144];
        for (i, blk) in b.as_chunks::<16>().0.iter().take(bw * bh).enumerate() {
            astc::pixels(&Block::read(blk, fp), fp, &mut px);
            for t in 0..(fp.0 * fp.1) as usize {
                let (x, y) =
                    (i % bw * fp.0 as usize + t % fp.0 as usize, y0 + i / bw * fp.1 as usize + t / fp.0 as usize);
                plane.set(x, y, px[t]);
            }
        }
    }

    fn upsampled(
        p: &Planes,
        _: Format,
        w: usize,
        rows: std::ops::Range<usize>,
        bufs: &mut Vec<[u8; 4]>,
        work: &mut Work,
    ) -> Planes {
        Planes { p: p.p.as_ref().map(|p| p.upsampled::<1>(w, rows, std::mem::take(bufs), &mut work.up)) }
    }

    fn stripe(
        s: &mut impl Side,
        md: &mut Models,
        f: Format,
        b: &mut [u8],
        (bw, bh): (usize, usize),
        up: &Planes,
        bufs: &mut Vec<[u8; 4]>,
        work: &mut Work,
    ) -> Result<Planes> {
        let fp = f.footprint();
        let fp = (fp.0 as u32, fp.1 as u32);
        let mut plane = Plane::new_in(std::mem::take(bufs), bw * fp.0 as usize, bh * fp.1 as usize);
        work.seen.clear();
        for by in 0..bh {
            for bx in 0..bw {
                let at = &mut b[(by * bw + bx) * 16..][..16];
                let raw: [u8; 16] = (*at).try_into().unwrap();
                let x = Self::block(s, md, &work.ranks, &raw, &work.seen, (bx, by, bw, fp), &mut plane, up.p.as_ref())?;
                at.copy_from_slice(&x.b.raw);
                md.copies.used(head(&x.b.raw, fp), x.b.raw);
                work.seen.push(x);
            }
        }
        Ok(Planes { p: Some(plane) })
    }
}
