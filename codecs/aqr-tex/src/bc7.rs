//! BC7 as a family of the walk ([`crate::texture`]; FORMAT.md, "`bc7-recons`"): each block as
//! the block to its left or above, or its head (all but its indices) as theirs or as a block's
//! before (the cache), or new: its fields ([`Block7`]) coded in turn, its mode and partition by
//! those of the blocks around, its endpoints from the pixels predicted (the next smaller
//! mipmap's, or those decoded around); then its indices as levels on the line between its
//! endpoints by where the pixels around predict them (as BC1's are). One plane of RGBA.

use aqr_common::arith::Bit;

use crate::Result;
use crate::bptc::{Block7, MODES, Mode, RESERVED};
use crate::coder::{Cat, Side};
use crate::copies::{Copies, Got, NEW, NONE};
use crate::layout::{Format, Group, Shape};
use crate::plane::{Plane, Up};
use crate::texture::Family;

pub struct Bc7;

/// The kinds of index sets, each of its own contexts: of 2, 3 and 4 bits of every channel, of
/// 2 and 3 bits of the colour alone, of 2 and 3 bits of the alpha alone (modes 4 and 5).
const KINDS: usize = 7;
/// The index bits of each kind.
const KIND_BITS: [u32; KINDS] = [2, 3, 4, 2, 3, 2, 3];

/// The buckets of a predicted level of indices of `bits` bits: a quarter of a level (of 2 bits)
/// or a half (of 3 and 4), and one each side of the line.
const fn buckets(bits: u32) -> usize {
    let steps = (1 << bits) - 1;
    steps
        * if bits == 2 {
            4
        } else if bits == 3 {
            2
        } else {
            1
        }
        + 3
}

/// Contrasts of a line (its length squared: up to 100, 1000, more).
const CONTRASTS: usize = 3;

/// Contexts of new endpoints' residuals: where the prediction comes from (3), the precision
/// (2), the channel (4), the endpoint (2), the residual before (4).
const RES: usize = 3 * 2 * 4 * 2 * 4;

/// Sizes of the index trees and of the flat ones (a line of no length) of each kind.
const fn idx_size(k: usize) -> usize {
    let (b, n) = (KIND_BITS[k], buckets(KIND_BITS[k]) + 1);
    CONTRASTS * n * n * (1 << b)
}

const fn flat_size(k: usize) -> usize {
    let (b, n) = (KIND_BITS[k], (1usize << KIND_BITS[k]) + 1);
    n * n * (1 << b)
}

const fn offsets(flat: bool) -> [usize; KINDS + 1] {
    let mut o = [0; KINDS + 1];
    let mut k = 0;
    while k < KINDS {
        o[k + 1] = o[k] + if flat { flat_size(k) } else { idx_size(k) };
        k += 1;
    }
    o
}

const IDX: [usize; KINDS + 1] = offsets(false);
const FLAT: [usize; KINDS + 1] = offsets(true);

/// The models of a texture, or of a stripe.
#[derive(Clone)]
pub struct Models {
    /// The mode by the modes of the blocks to the left and above (10 each: none or reserved).
    mode: Vec<Bit>,
    /// Whether the partition is the left block's, or the upper one's (of the same subsets).
    same: Vec<Bit>,
    /// The partition by its kind: of mode 0, of two subsets, of three.
    part: Vec<Bit>,
    rot: Vec<Bit>,
    isb: Vec<Bit>,
    res: Vec<Bit>,
    res_next: Vec<Bit>,
    pbit: Vec<Bit>,
    idx: Vec<Bit>,
    flat: Vec<Bit>,
    /// Blocks as those around or before.
    copies: Copies<16>,
}

impl Models {
    const SIZES: [usize; 10] = [100 * 16, 4, 3 * 64, 2 * 5 * 4, 3, RES * 16, RES * 16, 2 * 3, IDX[KINDS], FLAT[KINDS]];
}

/// The bits of block `b` but its indices (all of them of the reserved mode).
#[inline(always)]
fn head(b: &[u8; 16]) -> u128 {
    const IXBITS: [u32; 8] = [45, 46, 29, 30, 78, 62, 63, 30];
    let v = u128::from_le_bytes(*b);
    match b[0].trailing_zeros() {
        m @ 0..8 => v & ((1u128 << (128 - IXBITS[m as usize])) - 1),
        _ => v,
    }
}

/// Buffers of a surface's coding: the fields of its blocks, their bytes and their choices, and
/// an upsampling's.
pub struct Work {
    blocks: Vec<Block7>,
    raws: Vec<[u8; 16]>,
    choices: Vec<u8>,
    up: Up<u16>,
}

/// The planes of a surface: one of RGBA.
#[derive(Default)]
pub struct Planes {
    p: Option<Plane<u8, 4>>,
}

#[inline(always)]
fn zz(v: i32) -> u32 {
    ((v << 1) ^ (v >> 31)) as u32
}

#[inline(always)]
fn unzz(v: u32) -> i32 {
    (v >> 1) as i32 ^ -((v & 1) as i32)
}

/// `v` modulo 2^`bits`, in [−2^(bits−1), 2^(bits−1)).
#[inline(always)]
fn wrap(v: i32, bits: u32) -> i32 {
    (v << (32 - bits)) >> (32 - bits)
}

/// The class of a residual before (none or 0, 1, 2 or 3, more).
#[inline(always)]
fn class(r: i32) -> usize {
    match r.unsigned_abs() {
        0 => 0,
        1 => 1,
        2 | 3 => 2,
        _ => 3,
    }
}

/// A line between two endpoints, of which a pixel's bucket is where it projects on it.
#[derive(Clone, Copy)]
struct Line {
    dm: [i64; 4],
    off: i64,
    max: i64,
}

impl Line {
    /// The line from `e0` to `e1` over the channels of `mask`, in buckets of indices of
    /// `bits`; None of no length.
    #[inline(always)]
    fn new(e0: [u8; 4], e1: [u8; 4], mask: [bool; 4], bits: u32) -> (Option<Line>, usize) {
        let d: [i32; 4] = std::array::from_fn(|c| if mask[c] { e1[c] as i32 - e0[c] as i32 } else { 0 });
        let l2: i64 = d.iter().map(|&v| (v * v) as i64).sum();
        if l2 == 0 {
            return (None, 0);
        }
        let contrast = match l2 {
            1..=100 => 0,
            101..=1000 => 1,
            _ => 2,
        };
        let steps = (1i64 << bits) - 1;
        let res = if bits == 2 {
            4
        } else if bits == 3 {
            2
        } else {
            1
        };
        let m = ((steps * res) << 16) / l2;
        let dm = d.map(|v| v as i64 * m);
        let off = (1 << 15) - (0..4).map(|c| e0[c] as i64 * dm[c]).sum::<i64>();
        (Some(Line { dm, off, max: buckets(bits) as i64 - 1 }), contrast)
    }

    #[inline(always)]
    fn of(&self, v: [u8; 4]) -> usize {
        let proj = (0..4).map(|c| v[c] as i64 * self.dm[c]).sum::<i64>() + self.off;
        ((proj >> 16) + 1).clamp(0, self.max) as usize
    }
}

/// The predicted pixels of a block: of the next smaller mipmap, and of those decoded around
/// (the column to its left and the row above), each of the stored channels (alpha swapped back
/// by the rotation).
struct Around {
    coarse: Option<[[u8; 4]; 16]>,
    left: Option<[[u8; 4]; 4]>,
    top: Option<[[u8; 4]; 4]>,
}

impl Bc7 {
    fn mode_ctx(b: Option<&Block7>) -> usize {
        b.map_or(9, |b| b.mode as usize)
    }

    /// The partition of a block of mode `m` (when encoding, `part`), by those of the blocks
    /// to its left and above.
    #[allow(clippy::too_many_arguments)]
    fn partition(
        s: &mut impl Side,
        md: &mut Models,
        m: &Mode,
        mode: u8,
        part: u8,
        left: Option<&Block7>,
        up: Option<&Block7>,
    ) -> u8 {
        let fits = |b: Option<&Block7>| {
            b.filter(|b| b.mode != RESERVED && b.info().ns == m.ns && (b.partition as u32) < 1 << m.pb)
                .map(|b| (b.partition, b.mode == mode))
        };
        let (l, u) = (fits(left), fits(up));
        if let Some((p, same)) = l
            && s.bit(&mut md.same[same as usize], part == p, Cat::Mode)
        {
            return p;
        }
        if let Some((p, same)) = u.filter(|u| Some(u.0) != l.map(|l| l.0))
            && s.bit(&mut md.same[2 + same as usize], part == p, Cat::Mode)
        {
            return p;
        }
        let kind = match (m.ns, m.pb) {
            (_, 4) => 0,
            (2, _) => 1,
            _ => 2,
        };
        s.tree(&mut md.part[kind * 64..][..64], m.pb, part as u32, Cat::Mode) as u8
    }

    /// The predicted endpoints of subset `sub` of block `b` (its mode, partition and rotation
    /// known), as 8-bit channels as stored: where they come from (the smaller mipmap, the pixels
    /// around, none), and the two endpoints. (A neighbour's endpoints of the same layout predict
    /// worse: README, "BC7".)
    fn predicted(b: &Block7, sub: usize, a: &Around) -> (usize, [[u8; 4]; 2]) {
        let m = b.info();
        let (ax, ay) = (0..16).find(|&p| b.subset(p).0 == sub).map(|p| (p % 4, p / 4)).unwrap_or((0, 0));
        if let Some(c) = &a.coarse {
            // the pixels of the subset, the anchor's
            let (mut px, mut n) = ([[0u8; 4]; 16], 0);
            for p in 0..16 {
                if b.subset(p).0 == sub {
                    px[n] = c[p];
                    n += 1;
                }
            }
            return (0, Self::ends(m, &px[..n], c[ay * 4 + ax]));
        }
        Self::near(b, sub, a, m, (ax, ay))
    }

    /// The predicted endpoints of subset `sub` by the pixels decoded around the block, as
    /// [`Bc7::predicted`] gives them, the subset's anchor at (`ax`, `ay`).
    fn near(b: &Block7, sub: usize, a: &Around, m: &Mode, (ax, ay): (usize, usize)) -> (usize, [[u8; 4]; 2]) {
        let (mut px, mut n) = ([[0u8; 4]; 16], 0);
        // the pixels decoded around that touch the subset (or all of them where none do), the
        // one nearest the anchor
        for p in 0..16 {
            if b.subset(p).0 != sub {
                continue;
            }
            if let Some(l) = &a.left
                && p % 4 == 0
            {
                px[n] = l[p / 4];
                n += 1;
            }
            if let Some(t) = &a.top
                && p < 4
            {
                px[n] = t[p];
                n += 1;
            }
        }
        if n == 0 {
            for v in a.left.iter().chain(&a.top).flatten() {
                px[n] = *v;
                n += 1;
            }
        }
        let near = match (&a.left, &a.top) {
            (Some(l), Some(t)) => {
                if ax <= ay {
                    l[ay]
                } else {
                    t[ax]
                }
            }
            (Some(l), None) => l[ay],
            (None, Some(t)) => t[ax],
            (None, None) => return (2, [[128; 4]; 2]),
        };
        (1, Self::ends(m, &px[..n], near))
    }

    /// The ends of the line through pixels `px` (the colour and the alpha apart where they are of
    /// indices apart), the first the one nearer to pixel `anchor`.
    fn ends(m: &Mode, px: &[[u8; 4]], anchor: [u8; 4]) -> [[u8; 4]; 2] {
        let mut e = [[0u8; 4]; 2];
        let groups: &[&[usize]] = if m.ib2 > 0 { &[&[0, 1, 2], &[3]] } else { &[&[0, 1, 2, 3]] };
        for g in groups {
            let mean: [i32; 4] = std::array::from_fn(|k| {
                if g.contains(&k) { px.iter().map(|v| v[k] as i32).sum::<i32>() / px.len() as i32 } else { 0 }
            });
            let dist = |v: &[u8; 4]| g.iter().map(|&k| (v[k] as i32 - mean[k]).pow(2)).sum::<i32>();
            let far = px.iter().rev().max_by_key(|v| dist(v)).unwrap();
            let axis: [i32; 4] = std::array::from_fn(|k| if g.contains(&k) { far[k] as i32 - mean[k] } else { 0 });
            let proj = |v: &[u8; 4]| (0..4).map(|k| (v[k] as i32 - mean[k]) * axis[k]).sum::<i32>();
            let lo = px.iter().min_by_key(|v| proj(v)).unwrap();
            let hi = px.iter().rev().max_by_key(|v| proj(v)).unwrap();
            let (pa, plo, phi) = (proj(&anchor), proj(lo), proj(hi));
            let (e0, e1) = if (pa - plo).abs() <= (phi - pa).abs() { (lo, hi) } else { (hi, lo) };
            for &k in g.iter() {
                e[0][k] = e0[k];
                e[1][k] = e1[k];
            }
        }
        e
    }

    /// The endpoints of block `b` (its mode, partition and rotation known; when encoding,
    /// given), each subset's from its prediction, and their p-bits.
    fn endpoints(s: &mut impl Side, md: &mut Models, b: &mut Block7, want: &Block7, a: &Around) {
        let m = *b.info();
        let pb = (m.epb || m.spb) as u32;
        let prec = (m.cb >= 6) as usize;
        let mut pred_p = [2usize; 6];
        for sub in 0..m.ns {
            let (src, pred) = Self::predicted(b, sub, a);
            let mut r0 = [0i32; 4];
            for e in 0..2 {
                let j = 2 * sub + e;
                let mut rg = 0;
                // green, red, blue, then alpha
                for (slot, c) in [1usize, 0, 2, 3].into_iter().enumerate() {
                    let bits = if c < 3 { m.cb } else { m.ab };
                    if bits == 0 {
                        continue;
                    }
                    let full = bits + pb;
                    let q = (pred[e][c] as u32 * ((1 << full) - 1) + 127) / 255;
                    if c == 1 && pb == 1 {
                        pred_p[j] = (q & 1) as usize;
                    }
                    let mut p = (q >> pb) as i32;
                    if c == 0 || c == 2 {
                        p += rg;
                    }
                    let before = match (c, e) {
                        (0 | 2, _) => class(rg),
                        (_, 1) => class(r0[c]),
                        _ => 0,
                    };
                    let ctx = ((((src * 2 + prec) * 4 + slot) * 2 + e) * 4 + before) * 16;
                    let v = zz(wrap(want.ep[j][c] as i32 - p, bits));
                    let v = s.token(&mut md.res[ctx..ctx + 16], &mut md.res_next[ctx..ctx + 16], v, Cat::Endpoints);
                    // (a value past the bits wraps: any value is some endpoint)
                    let r = wrap(unzz(v), bits);
                    b.ep[j][c] = ((p + r) & ((1 << bits) - 1)) as u8;
                    if c == 1 {
                        rg = r;
                    }
                    if e == 0 {
                        r0[c] = r;
                    }
                }
            }
        }
        if m.epb {
            for j in 0..2 * m.ns {
                b.p[j] = s.bit(&mut md.pbit[pred_p[j]], want.p[j] != 0, Cat::Endpoints) as u8;
            }
        } else if m.spb {
            for sub in 0..m.ns {
                let v = s.bit(&mut md.pbit[3 + pred_p[2 * sub]], want.p[2 * sub] != 0, Cat::Endpoints) as u8;
                (b.p[2 * sub], b.p[2 * sub + 1]) = (v, v);
            }
        }
    }

    /// The indices of block `b` at (`bx`, `by`) (when encoding, `want`'s), its pixels set in
    /// `plane` as they decode, `coarse` the plane of the smaller mipmap upsampled.
    #[allow(clippy::too_many_arguments)]
    fn indices(
        s: &mut impl Side,
        md: &mut Models,
        b: &mut Block7,
        want: &Block7,
        plane: &mut Plane<u8, 4>,
        coarse: Option<&Plane<u8, 4>>,
        bx: usize,
        by: usize,
    ) {
        let m = *b.info();
        let e: [[u8; 4]; 6] = std::array::from_fn(|j| if j < 2 * m.ns { b.endpoint(j) } else { [0; 4] });
        // the index sets: (index bits, kind, channels) of the first, and of the second
        let (one, two) = match (b.mode, b.isb) {
            (0 | 1, _) => ((3, 1), None),
            (6, _) => ((4, 2), None),
            (4, 0) => ((2, 3), Some((3, 6))),
            (4, _) => ((2, 5), Some((3, 4))),
            (5, _) => ((2, 3), Some((2, 5))),
            _ => ((2, 0), None),
        };
        let mask = |k: usize| match k {
            3 | 4 => [true, true, true, false],
            5 | 6 => [false, false, false, true],
            _ => [true; 4],
        };
        let lines: [[(Option<Line>, usize); 3]; 2] = std::array::from_fn(|set| {
            std::array::from_fn(|sub| {
                let (bits, kind) = if set == 0 { one } else { two.unwrap_or(one) };
                if sub < m.ns { Line::new(e[2 * sub], e[2 * sub + 1], mask(kind), bits) } else { (None, 0) }
            })
        });
        let w1 = crate::bptc::WEIGHTS[m.ib as usize];
        let w2 = crate::bptc::WEIGHTS[m.ib2.max(2) as usize];
        let unrot = |mut v: [u8; 4]| {
            crate::bptc::rotate(&mut v, b.rotation);
            v
        };
        for p in 0..16 {
            let (x, y) = (bx * 4 + p % 4, by * 4 + p / 4);
            let (sub, anchor) = b.subset(p);
            // (of no smaller mipmap, the pixels to the left and above apart)
            let (q1, q2) = match coarse {
                Some(c) => (Some(unrot(c.at(x, y))), plane.causal(x, y).map(unrot)),
                None => ((y > 0).then(|| unrot(plane.at(x, y - 1))), (x > 0).then(|| unrot(plane.at(x - 1, y)))),
            };
            let mut got = [0u8; 2];
            for set in 0..1 + two.is_some() as usize {
                let (bits, kind) = if set == 0 { one } else { two.unwrap() };
                let v = if set == 0 { want.ix[p] } else { want.ix2[p] } as u32;
                // (an anchor's top bit is 0: the subtree of node 2; pixel 0 is the second set's)
                let lead = if set == 0 { anchor } else { p == 0 };
                let (root, n) = if lead { (2, bits - 1) } else { (1, bits) };
                let (line, contrast) = lines[set][sub];
                let i = match line {
                    Some(l) => {
                        let nb = buckets(bits) + 1;
                        let a = q1.map_or(nb - 1, |v| l.of(v));
                        let c = q2.map_or(nb - 1, |v| l.of(v));
                        let ctx = IDX[kind] + ((contrast * nb + a) * nb + c) * (1 << bits);
                        s.tree_at(&mut md.idx[ctx..ctx + (1 << bits)], root, n, v, Cat::Indices)
                    }
                    None => {
                        // (no line: by the indices to the left and above in the block)
                        let ix = if set == 0 { &b.ix } else { &b.ix2 };
                        let left = if p % 4 > 0 { ix[p - 1] as usize + 1 } else { 0 };
                        let above = if p >= 4 { ix[p - 4] as usize + 1 } else { 0 };
                        let n1 = (1 << bits) + 1;
                        let ctx = FLAT[kind] + (left * n1 + above) * (1 << bits);
                        s.tree_at(&mut md.flat[ctx..ctx + (1 << bits)], root, n, v, Cat::Indices)
                    }
                };
                got[set] = i as u8;
                if set == 0 {
                    b.ix[p] = i as u8;
                } else {
                    b.ix2[p] = i as u8;
                }
            }
            let (a, z) = (e[2 * sub], e[2 * sub + 1]);
            let lerp = crate::bptc::lerp;
            let mut px: [u8; 4] = match two {
                None => std::array::from_fn(|c| lerp(a[c], z[c], w1[got[0] as usize])),
                Some(_) => {
                    let (wc, wa) = match b.isb {
                        0 => (w1[got[0] as usize], w2[got[1] as usize]),
                        _ => (w2[got[1] as usize], w1[got[0] as usize]),
                    };
                    let mut v: [u8; 4] = std::array::from_fn(|c| lerp(a[c], z[c], wc));
                    v[3] = lerp(a[3], z[3], wa);
                    v
                }
            };
            crate::bptc::rotate(&mut px, b.rotation);
            plane.set(x, y, px);
        }
    }

    /// The pixels of block `b` set in `plane` at (`bx`, `by`).
    fn set(plane: &mut Plane<u8, 4>, b: &Block7, bx: usize, by: usize) {
        let px = b.pixels();
        for p in 0..16 {
            plane.set(bx * 4 + p % 4, by * 4 + p / 4, px[p]);
        }
    }

    /// Codes the block at (`bx`, `by`) of a surface `bw` blocks wide (when encoding, `raw`, of
    /// fields `want`): its fields, its bytes and its choice.
    #[allow(clippy::too_many_arguments)]
    fn block(
        s: &mut impl Side,
        md: &mut Models,
        (want, raw): (&Block7, &[u8; 16]),
        w: &Work,
        (bx, by, bw): (usize, usize, usize),
        plane: &mut Plane<u8, 4>,
        coarse: Option<&Plane<u8, 4>>,
    ) -> Result<(Block7, [u8; 16], u8)> {
        let i = by * bw + bx;
        let left = (bx > 0).then(|| (&w.blocks[i - 1], &w.raws[i - 1]));
        let up = (by > 0).then(|| (&w.blocks[i - bw], &w.raws[i - bw]));
        let lc = if bx > 0 { w.choices[i - 1] } else { NONE };
        let uc = if by > 0 { w.choices[i - bw] } else { NONE };
        match md.copies.code(s, raw, left.map(|l| l.1), up.map(|u| u.1), lc, uc, head)? {
            Got::Block(c, choice) => {
                // (of the left or upper block, its fields as they are)
                let b = match choice {
                    crate::copies::BLOCK_LEFT => *left.unwrap().0,
                    crate::copies::BLOCK_UP => *up.unwrap().0,
                    _ => Block7::read(&c),
                };
                Self::set(plane, &b, bx, by);
                return Ok((b, c, choice));
            }
            Got::Head(c, choice) => {
                let mut b = Block7::read(&c);
                if b.mode == RESERVED {
                    // (a head of the reserved mode is all of its block)
                    Self::set(plane, &b, bx, by);
                    return Ok((b, c, choice));
                }
                Self::indices(s, md, &mut b, want, plane, coarse, bx, by);
                return Ok((b, b.write(), choice));
            }
            Got::New => {}
        }
        let (left, up) = (left.map(|l| l.0), up.map(|u| u.0));
        let ctx = (Self::mode_ctx(left) * 10 + Self::mode_ctx(up)) * 16;
        let mode = s.tree(&mut md.mode[ctx..ctx + 16], 4, want.mode as u32, Cat::Mode).min(8) as u8;
        let mut b = Block7 { mode, ..Default::default() };
        if mode == RESERVED {
            // (the bytes after the first, as they are; black and transparent pixels)
            for k in 1..16 {
                b.raw[k] = s.raw(8, want.raw[k] as u32, Cat::Mode) as u8;
            }
            for p in 0..16 {
                plane.set(bx * 4 + p % 4, by * 4 + p / 4, [0; 4]);
            }
            return Ok((b, b.raw, NEW));
        }
        let m = MODES[mode as usize];
        if m.ns > 1 {
            b.partition = Self::partition(s, md, &m, mode, want.partition, left, up);
        }
        if m.rb > 0 {
            let lr = left.filter(|l| l.mode == mode).map_or(4, |l| l.rotation as usize);
            let ctx = ((mode as usize - 4) * 5 + lr) * 4;
            b.rotation = s.tree(&mut md.rot[ctx..ctx + 4], 2, want.rotation as u32, Cat::Mode) as u8;
        }
        if m.isb > 0 {
            let li = left.filter(|l| l.mode == mode).map_or(2, |l| l.isb as usize);
            b.isb = s.bit(&mut md.isb[li], want.isb != 0, Cat::Mode) as u8;
        }
        let unrot = |mut v: [u8; 4]| {
            crate::bptc::rotate(&mut v, b.rotation);
            v
        };
        let around = Around {
            coarse: coarse.map(|c| std::array::from_fn(|p| unrot(c.at(bx * 4 + p % 4, by * 4 + p / 4)))),
            left: (bx > 0).then(|| std::array::from_fn(|k| unrot(plane.at(bx * 4 - 1, by * 4 + k)))),
            top: (by > 0).then(|| std::array::from_fn(|k| unrot(plane.at(bx * 4 + k, by * 4 - 1)))),
        };
        Self::endpoints(s, md, &mut b, want, &around);
        Self::indices(s, md, &mut b, want, plane, coarse, bx, by);
        Ok((b, b.write(), NEW))
    }
}

impl Family for Bc7 {
    const GROUP: Group = Group::Bc7;

    #[inline(always)]
    fn shape(_: Format) -> Shape {
        Shape { fw: 4, fh: 4, least: 1, bytes: 16 }
    }

    type Models = Models;
    type Planes = Planes;
    type Bufs = Vec<[u8; 4]>;
    type Work = Work;

    fn models(_: Format) -> Models {
        let v = |n| vec![Bit::default(); n];
        let [mode, same, part, rot, isb, res, res_next, pbit, idx, flat] = Models::SIZES;
        Models {
            mode: v(mode),
            same: v(same),
            part: v(part),
            rot: v(rot),
            isb: v(isb),
            res: v(res),
            res_next: v(res_next),
            pbit: v(pbit),
            idx: v(idx),
            flat: v(flat),
            copies: Copies::new(),
        }
    }

    fn models_plan(_: Format) -> u64 {
        let bits: u64 = Models::SIZES.iter().map(|&n| aqr_api::stack::block((n * size_of::<Bit>()) as u64)).sum();
        bits + Copies::<16>::plan()
    }

    fn copy_models(m: &mut Models, o: &Models) {
        m.mode.clone_from(&o.mode);
        m.same.clone_from(&o.same);
        m.part.clone_from(&o.part);
        m.rot.clone_from(&o.rot);
        m.isb.clone_from(&o.isb);
        m.res.clone_from(&o.res);
        m.res_next.clone_from(&o.res_next);
        m.pbit.clone_from(&o.pbit);
        m.idx.clone_from(&o.idx);
        m.flat.clone_from(&o.flat);
        m.copies.copy_from(&o.copies);
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
        Work {
            blocks: Vec::with_capacity(blocks),
            raws: Vec::with_capacity(blocks),
            choices: Vec::with_capacity(blocks),
            up: Up::with_capacity(cols, across),
        }
    }

    fn work_plan(_: Format, blocks: usize, cols: usize, across: usize) -> u64 {
        use aqr_api::stack::block;
        let n = blocks as u64;
        block(n * size_of::<Block7>() as u64) + block(16 * n) + block(n) + Up::<u16>::plan(cols, across)
    }

    fn across_values(_: Format) -> usize {
        4
    }

    fn planes_of(_: Format, b: &[u8], (bw, bh): (usize, usize), p: &mut Planes) {
        let plane = p.p.as_mut().expect("a plane");
        let y0 = plane.grow(bh * 4);
        for (i, b) in b.as_chunks::<16>().0.iter().take(bw * bh).enumerate() {
            let px = Block7::read(b).pixels();
            for q in 0..16 {
                plane.set(i % bw * 4 + q % 4, y0 + i / bw * 4 + q / 4, px[q]);
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
        Planes { p: p.p.as_ref().map(|p| p.upsampled::<4>(w, rows, std::mem::take(bufs), &mut work.up)) }
    }

    fn stripe(
        s: &mut impl Side,
        md: &mut Models,
        _: Format,
        b: &mut [u8],
        (bw, bh): (usize, usize),
        up: &Planes,
        bufs: &mut Vec<[u8; 4]>,
        work: &mut Work,
    ) -> Result<Planes> {
        let mut plane = Plane::new_in(std::mem::take(bufs), bw * 4, bh * 4);
        work.blocks.clear();
        work.raws.clear();
        work.choices.clear();
        let enc = s.encoding();
        for by in 0..bh {
            for bx in 0..bw {
                let at = &mut b[(by * bw + bx) * 16..][..16];
                let raw: [u8; 16] = (*at).try_into().unwrap();
                let want = if enc { Block7::read(&raw) } else { Block7::default() };
                let (got, bytes, choice) =
                    Self::block(s, md, (&want, &raw), work, (bx, by, bw), &mut plane, up.p.as_ref())?;
                at.copy_from_slice(&bytes);
                md.copies.used(head(&bytes), bytes);
                work.blocks.push(got);
                work.raws.push(bytes);
                work.choices.push(choice);
            }
        }
        Ok(Planes { p: Some(plane) })
    }
}
