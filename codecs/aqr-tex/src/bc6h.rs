//! BC6H as a family of the walk ([`crate::texture`]; FORMAT.md, "`bc6h-recons`"), as BC7 is
//! ([`crate::bc7`]): each block as the block to its left or above, or its head (all but its
//! indices) as theirs or as a block's before, or new: its mode and partition by those of the
//! blocks around, its endpoints (the first, and the others as stored: differences from it, of
//! transformed modes) from the pixels predicted, its indices as levels on the line between its
//! endpoints. One plane of RGB, each channel of 16 bits: the endpoints unquantized and
//! interpolated (before they are scaled to half floats; 0x8000 added of signed formats).

use aqr_common::arith::Bit;

use crate::Result;
use crate::bptc::{Block6, MODES6, Mode6, RESERVED6, quantize};
use crate::coder::{Cat, Side, bit_models, unzz, wide_token, wrap, zz};
use crate::copies::{BLOCK_LEFT, BLOCK_UP, Copies, Got, NEW, NONE};
use crate::layout::{Format, Group, Shape};
use crate::plane::{Plane, Up};
use crate::texture::Family;

pub struct Bc6h;

/// The buckets of a predicted level of indices of 3 bits (a half of a level) and of 4 (a
/// level), and one each side of the line.
const fn buckets(bits: u32) -> usize {
    if bits == 3 { 7 * 2 + 3 } else { 15 + 3 }
}

const CONTRASTS: usize = 3;

/// Contexts of new endpoints' residuals: where the prediction comes from (3), the precision
/// (3), the endpoint (4), the channel (3), the residual before (8).
const RES: usize = 3 * 3 * 4 * 3 * 8;

/// The index trees of indices of 3 bits and of 4, then the flat ones (a line of no length).
const IDX3: usize = CONTRASTS * (buckets(3) + 1) * (buckets(3) + 1) * 8;
const IDX4: usize = CONTRASTS * (buckets(4) + 1) * (buckets(4) + 1) * 16;
const FLAT3: usize = 9 * 9 * 8;
const FLAT4: usize = 17 * 17 * 16;

bit_models! {
    /// The models of a texture, or of a stripe.
    #[derive(Clone)]
    pub struct Models {
        /// The mode by the modes of the blocks to the left and above (16 each: reserved, none).
        mode: 256 * 16,
        /// Whether the partition is the left block's, or the upper one's (of two subsets).
        same: 4,
        part: 32,
        res: RES * 32,
        res_next: RES * 32,
        idx: IDX3 + IDX4,
        flat: FLAT3 + FLAT4;
        copies: Copies<16>,
    }
}

/// The bits of block `b` but its indices (all of them of a reserved mode).
#[inline(always)]
fn head(b: &[u8; 16]) -> u128 {
    let v = u128::from_le_bytes(*b);
    match Block6::mode_of(b[0]) {
        RESERVED6 => v,
        m if MODES6[m as usize].ns == 2 => v & ((1u128 << 82) - 1),
        _ => v & ((1u128 << 65) - 1),
    }
}

/// Buffers of a surface's coding: the fields of its blocks, their bytes and their choices, and
/// an upsampling's.
pub struct Work {
    blocks: Vec<Block6>,
    raws: Vec<[u8; 16]>,
    choices: Vec<u8>,
    up: Up<u32>,
}

/// The planes of a surface: one of RGB.
#[derive(Default)]
pub struct Planes {
    p: Option<Plane<u16, 3>>,
}

/// The class of a residual before: its bit length, up to 7.
#[inline(always)]
fn class(r: i32) -> usize {
    (32 - r.unsigned_abs().leading_zeros()).min(7) as usize
}

/// A line between two endpoints (of the plane's values), of which a pixel's bucket is where it
/// projects on it.
#[derive(Clone, Copy)]
struct Line {
    e0: [i64; 3],
    d: [i64; 3],
    l2: i64,
    scale: i64,
    max: i64,
}

impl Line {
    /// The line from `e0` to `e1`, in buckets of indices of `bits`; None of no length.
    #[inline(always)]
    fn new(e0: [u16; 3], e1: [u16; 3], bits: u32) -> (Option<Line>, usize) {
        let d: [i64; 3] = std::array::from_fn(|c| e1[c] as i64 - e0[c] as i64);
        let l2: i64 = d.iter().map(|&v| v * v).sum();
        if l2 == 0 {
            return (None, 0);
        }
        let contrast = match l2 {
            1..=10_000 => 0,
            10_001..=1_000_000 => 1,
            _ => 2,
        };
        let scale = if bits == 3 { 7 * 2 } else { 15 };
        (Some(Line { e0: e0.map(|v| v as i64), d, l2, scale, max: buckets(bits) as i64 - 1 }), contrast)
    }

    #[inline(always)]
    fn of(&self, v: [u16; 3]) -> usize {
        let dot: i64 = (0..3).map(|c| (v[c] as i64 - self.e0[c]) * self.d[c]).sum();
        ((dot * self.scale + self.l2 / 2).div_euclid(self.l2) + 1).clamp(0, self.max) as usize
    }
}

/// The predicted pixels of a block: of the next smaller mipmap, and of those decoded around (the
/// column to its left and the row above).
struct Around {
    coarse: Option<[[u16; 3]; 16]>,
    left: Option<[[u16; 3]; 4]>,
    top: Option<[[u16; 3]; 4]>,
}

impl Bc6h {
    fn mode_ctx(b: Option<&Block6>) -> usize {
        b.map_or(15, |b| b.mode as usize)
    }

    /// The partition of a block of two subsets (when encoding, `part`), by those of the blocks to
    /// its left and above.
    fn partition(
        s: &mut impl Side,
        md: &mut Models,
        mode: u8,
        part: u8,
        left: Option<&Block6>,
        up: Option<&Block6>,
    ) -> u8 {
        let fits = |b: Option<&Block6>| {
            b.filter(|b| b.mode != RESERVED6 && b.info().ns == 2).map(|b| (b.partition, b.mode == mode))
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
        s.tree(&mut md.part, 5, part as u32, Cat::Mode) as u8
    }

    /// The predicted endpoints of subset `sub` of block `b` (its mode and partition known), of
    /// the plane's values: where they come from (the smaller mipmap, the pixels around, none), and
    /// the two endpoints.
    fn predicted(b: &Block6, sub: usize, a: &Around, mid: u16) -> (usize, [[u16; 3]; 2]) {
        let (ax, ay) = (0..16).find(|&p| b.subset(p).0 == sub).map(|p| (p % 4, p / 4)).unwrap_or((0, 0));
        let (mut px, mut n) = ([[0u16; 3]; 16], 0);
        if let Some(c) = &a.coarse {
            for p in 0..16 {
                if b.subset(p).0 == sub {
                    px[n] = c[p];
                    n += 1;
                }
            }
            return (0, ends(&px[..n], c[ay * 4 + ax]));
        }
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
            (None, None) => return (2, [[mid; 3]; 2]),
        };
        (1, ends(&px[..n], near))
    }

    /// The endpoints of block `b` (its mode and partition known; when encoding, `want`'s), each
    /// subset's from its prediction.
    fn endpoints(s: &mut impl Side, md: &mut Models, b: &mut Block6, want: &Block6, a: &Around, signed: bool) {
        let m: Mode6 = *b.info();
        let bias = if signed { 0x8000 } else { 0 };
        let prec = match m.w {
            ..=8 => 0,
            9..=11 => 1,
            _ => 2,
        };
        let mut r0 = [0i32; 3];
        for sub in 0..m.ns {
            let (src, pred) = Self::predicted(b, sub, a, 0x8000);
            for e in 0..2 {
                let j = 2 * sub + e;
                let mut rg = 0;
                // green, red, then blue
                for (slot, c) in [1usize, 0, 2].into_iter().enumerate() {
                    let q = quantize(pred[e][c] as i32 - bias, m.w, signed);
                    // (the first endpoint's channel as it is, the others' as stored: differences
                    // from the first's of transformed modes)
                    let bits = if j == 0 { m.w } else { m.d[c] };
                    let mut p = match (j, m.transformed) {
                        (0, _) | (_, false) => q,
                        _ => q - b.integer(0, signed)[c],
                    };
                    if c != 1 {
                        p += rg;
                    }
                    let before = match (c, j) {
                        (0 | 2, _) => class(rg),
                        (_, 0) => 0,
                        _ => class(r0[c]),
                    };
                    let ctx = ((((src * 3 + prec) * 4 + j) * 3 + slot) * 8 + before) * 32;
                    let v = zz(wrap(want.v[j][c] as i32 - p, bits));
                    let v =
                        wide_token(s, &mut md.res[ctx..ctx + 32], &mut md.res_next[ctx..ctx + 32], v, Cat::Endpoints);
                    let r = wrap(unzz(v), bits);
                    b.v[j][c] = ((p + r) & ((1 << bits) - 1)) as u16;
                    if c == 1 {
                        rg = r;
                    }
                    if j == 0 {
                        r0[c] = r;
                    }
                }
            }
        }
    }

    /// The indices of block `b` at (`bx`, `by`) (when encoding, `want`'s), its pixels set in
    /// `plane` as they decode, `coarse` the plane of the smaller mipmap upsampled.
    #[allow(clippy::too_many_arguments)]
    fn indices(
        s: &mut impl Side,
        md: &mut Models,
        b: &mut Block6,
        want: &Block6,
        plane: &mut Plane<u16, 3>,
        coarse: Option<&Plane<u16, 3>>,
        (bx, by): (usize, usize),
        signed: bool,
    ) {
        let m = *b.info();
        let bias = if signed { 0x8000 } else { 0 };
        let e: [[u16; 3]; 4] =
            std::array::from_fn(
                |j| {
                    if j < 2 * m.ns { b.endpoint(j, signed).map(|v| (v + bias) as u16) } else { [0; 3] }
                },
            );
        let bits = b.index_bits();
        let lines: [(Option<Line>, usize); 2] =
            std::array::from_fn(|sub| if sub < m.ns { Line::new(e[2 * sub], e[2 * sub + 1], bits) } else { (None, 0) });
        let w = crate::bptc::WEIGHTS[bits as usize];
        let (base, fbase) = if bits == 3 { (0, 0) } else { (IDX3, FLAT3) };
        for p in 0..16 {
            let (x, y) = (bx * 4 + p % 4, by * 4 + p / 4);
            let (sub, anchor) = b.subset(p);
            let (q1, q2) = match coarse {
                Some(c) => (Some(c.at(x, y)), plane.causal(x, y)),
                None => ((y > 0).then(|| plane.at(x, y - 1)), (x > 0).then(|| plane.at(x - 1, y))),
            };
            let (root, n) = if anchor { (2, bits - 1) } else { (1, bits) };
            let v = want.ix[p] as u32;
            let (line, contrast) = lines[sub];
            let i = match line {
                Some(l) => {
                    let nb = buckets(bits) + 1;
                    let a = q1.map_or(nb - 1, |v| l.of(v));
                    let c = q2.map_or(nb - 1, |v| l.of(v));
                    let ctx = base + ((contrast * nb + a) * nb + c) * (1 << bits);
                    s.tree_at(&mut md.idx[ctx..ctx + (1 << bits)], root, n, v, Cat::Indices)
                }
                None => {
                    let left = if p % 4 > 0 { b.ix[p - 1] as usize + 1 } else { 0 };
                    let above = if p >= 4 { b.ix[p - 4] as usize + 1 } else { 0 };
                    let n1 = (1 << bits) + 1;
                    let ctx = fbase + (left * n1 + above) * (1 << bits);
                    s.tree_at(&mut md.flat[ctx..ctx + (1 << bits)], root, n, v, Cat::Indices)
                }
            };
            b.ix[p] = i as u8;
            let (a, z, wt) = (e[2 * sub], e[2 * sub + 1], w[i as usize] as i32);
            plane.set(x, y, std::array::from_fn(|c| ((a[c] as i32 * (64 - wt) + z[c] as i32 * wt + 32) >> 6) as u16));
        }
    }

    /// The pixels of block `b` set in `plane` at (`bx`, `by`).
    fn set(plane: &mut Plane<u16, 3>, b: &Block6, bx: usize, by: usize, signed: bool) {
        let px = b.pixels(signed);
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
        (want, raw): (&Block6, &[u8; 16]),
        w: &Work,
        (bx, by, bw): (usize, usize, usize),
        plane: &mut Plane<u16, 3>,
        coarse: Option<&Plane<u16, 3>>,
        signed: bool,
    ) -> Result<(Block6, [u8; 16], u8)> {
        let i = by * bw + bx;
        let left = (bx > 0).then(|| (&w.blocks[i - 1], &w.raws[i - 1]));
        let up = (by > 0).then(|| (&w.blocks[i - bw], &w.raws[i - bw]));
        let lc = if bx > 0 { w.choices[i - 1] } else { NONE };
        let uc = if by > 0 { w.choices[i - bw] } else { NONE };
        match md.copies.code(s, raw, left.map(|l| l.1), up.map(|u| u.1), lc, uc, head)? {
            Got::Block(c, choice) => {
                let b = match choice {
                    BLOCK_LEFT => *left.unwrap().0,
                    BLOCK_UP => *up.unwrap().0,
                    _ => Block6::read(&c),
                };
                Self::set(plane, &b, bx, by, signed);
                return Ok((b, c, choice));
            }
            Got::Head(c, choice) => {
                let mut b = Block6::read(&c);
                if b.mode == RESERVED6 {
                    Self::set(plane, &b, bx, by, signed);
                    return Ok((b, c, choice));
                }
                Self::indices(s, md, &mut b, want, plane, coarse, (bx, by), signed);
                return Ok((b, b.write(), choice));
            }
            Got::New => {}
        }
        let (left, up) = (left.map(|l| l.0), up.map(|u| u.0));
        let ctx = (Self::mode_ctx(left) * 16 + Self::mode_ctx(up)) * 16;
        let mode = s.tree(&mut md.mode[ctx..ctx + 16], 4, want.mode as u32, Cat::Mode).min(RESERVED6 as u32) as u8;
        let mut b = Block6 { mode, ..Default::default() };
        if mode == RESERVED6 {
            // (its header, then the bits after it, as they are)
            let h = s.raw(5, want.raw[0] as u32 & 31, Cat::Mode) as u8;
            let reserved = [0b10011, 0b10111, 0b11011, 0b11111];
            b.raw[0] = if reserved.contains(&h) { h } else { 0b10011 };
            b.raw[0] |= (s.raw(3, want.raw[0] as u32 >> 5, Cat::Mode) as u8) << 5;
            for k in 1..16 {
                b.raw[k] = s.raw(8, want.raw[k] as u32, Cat::Mode) as u8;
            }
            Self::set(plane, &b, bx, by, signed);
            return Ok((b, b.raw, NEW));
        }
        if MODES6[mode as usize].ns == 2 {
            b.partition = Self::partition(s, md, mode, want.partition, left, up);
        }
        let around = Around {
            coarse: coarse.map(|c| std::array::from_fn(|p| c.at(bx * 4 + p % 4, by * 4 + p / 4))),
            left: (bx > 0).then(|| std::array::from_fn(|k| plane.at(bx * 4 - 1, by * 4 + k))),
            top: (by > 0).then(|| std::array::from_fn(|k| plane.at(bx * 4 + k, by * 4 - 1))),
        };
        Self::endpoints(s, md, &mut b, want, &around, signed);
        Self::indices(s, md, &mut b, want, plane, coarse, (bx, by), signed);
        Ok((b, b.write(), NEW))
    }
}

/// The ends of the line through pixels `px`, the first the one nearer to pixel `anchor` (as BC7's
/// are: FORMAT.md).
fn ends(px: &[[u16; 3]], anchor: [u16; 3]) -> [[u16; 3]; 2] {
    let n = px.len() as i64;
    let mean: [i64; 3] = std::array::from_fn(|k| px.iter().map(|v| v[k] as i64).sum::<i64>() / n);
    let dist = |v: &[u16; 3]| (0..3).map(|k| (v[k] as i64 - mean[k]).pow(2)).sum::<i64>();
    let far = px.iter().rev().max_by_key(|v| dist(v)).unwrap();
    let axis: [i64; 3] = std::array::from_fn(|k| far[k] as i64 - mean[k]);
    let proj = |v: &[u16; 3]| (0..3).map(|k| (v[k] as i64 - mean[k]) * axis[k]).sum::<i64>();
    let lo = px.iter().min_by_key(|v| proj(v)).unwrap();
    let hi = px.iter().rev().max_by_key(|v| proj(v)).unwrap();
    let (pa, plo, phi) = (proj(&anchor), proj(lo), proj(hi));
    if (pa - plo).abs() <= (phi - pa).abs() { [*lo, *hi] } else { [*hi, *lo] }
}

impl Family for Bc6h {
    const GROUP: Group = Group::Bc6h;

    #[inline(always)]
    fn shape(_: Format) -> Shape {
        Shape { fw: 4, fh: 4, least: 1, bytes: 16 }
    }

    type Models = Models;
    type Planes = Planes;
    type Bufs = Vec<[u16; 3]>;
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

    fn bufs(_: Format, px: usize) -> Vec<[u16; 3]> {
        Vec::with_capacity(px)
    }

    fn bufs_plan(_: Format, px: usize) -> u64 {
        aqr_api::stack::block(6 * px as u64)
    }

    fn give(b: &mut Vec<[u16; 3]>, p: Planes) {
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
        block(n * size_of::<Block6>() as u64) + block(16 * n) + block(n) + Up::<u32>::plan(cols, across)
    }

    fn across_values(_: Format) -> usize {
        3
    }

    fn planes_of(f: Format, b: &[u8], (bw, bh): (usize, usize), p: &mut Planes) {
        let signed = f == (Format::Bc6h { signed: true });
        let plane = p.p.as_mut().expect("a plane");
        let y0 = plane.grow(bh * 4);
        for (i, b) in b.as_chunks::<16>().0.iter().take(bw * bh).enumerate() {
            let px = Block6::read(b).pixels(signed);
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
        bufs: &mut Vec<[u16; 3]>,
        work: &mut Work,
    ) -> Planes {
        Planes { p: p.p.as_ref().map(|p| p.upsampled::<4>(w, rows, std::mem::take(bufs), &mut work.up)) }
    }

    fn stripe(
        s: &mut impl Side,
        md: &mut Models,
        f: Format,
        b: &mut [u8],
        (bw, bh): (usize, usize),
        up: &Planes,
        bufs: &mut Vec<[u16; 3]>,
        work: &mut Work,
    ) -> Result<Planes> {
        let signed = f == (Format::Bc6h { signed: true });
        let mut plane = Plane::new_in(std::mem::take(bufs), bw * 4, bh * 4);
        work.blocks.clear();
        work.raws.clear();
        work.choices.clear();
        let enc = s.encoding();
        for by in 0..bh {
            for bx in 0..bw {
                let at = &mut b[(by * bw + bx) * 16..][..16];
                let raw: [u8; 16] = (*at).try_into().unwrap();
                let want = if enc { Block6::read(&raw) } else { Block6::default() };
                let (got, bytes, choice) =
                    Self::block(s, md, (&want, &raw), work, (bx, by, bw), &mut plane, up.p.as_ref(), signed)?;
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
