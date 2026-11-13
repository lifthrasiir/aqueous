//! ETC1, ETC2 and EAC as a family of the walk ([`crate::texture`]; FORMAT.md, "`etc-recons`"):
//! the parts of their blocks ([`crate::etc`]), RGB (ETC1, ETC2 and its punch-through alpha) and
//! EAC (ETC2's alpha, R11, RG11's two), each part over a surface at a time. A part's block is
//! the block to its left or above, or its head (all but its indices) theirs or a block's before
//! ([`Copies`]), or new: its mode and fields predicted from the pixels (the next smaller
//! mipmap's, or those decoded around), the bits of no field as they are; then its indices as
//! levels by where the pixels around predict them, on a *ladder* of the values of its levels.
//! Planes: of RGBA (8 bits) of RGB parts, of values (16 bits) of EAC parts.

use aqr_common::arith::Bit;

use crate::Result;
use crate::coder::{Cat, Side};
use crate::copies::{Copies, Got, NEW, NONE};
use crate::etc::{self, DIFFERENTIAL, Eac, FIELDS, H, INDIVIDUAL, PLANAR, T};
use crate::layout::{Format, Group, Shape};
use crate::plane::{Plane, Up};
use crate::texture::Family;

pub struct Etc;

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

/// The class of a residual before (0, 1, 2 or 3, more).
#[inline(always)]
fn class(r: i32) -> usize {
    match r.unsigned_abs() {
        0 => 0,
        1 => 1,
        2 | 3 => 2,
        _ => 3,
    }
}

/// The bucket of `x` on the ladder of sorted values `v`: the thresholds it is at or above, of
/// the values and the midpoints between them (0 to 2·n − 1).
#[inline(always)]
fn rung(v: &[i32], x: i32) -> usize {
    let mut b = 0;
    for i in 0..v.len() {
        b += (x >= v[i]) as usize;
        if i + 1 < v.len() {
            b += (2 * x >= v[i] + v[i + 1]) as usize;
        }
    }
    b
}

/// The mean of `px` (none of no pixels).
fn mean<const C: usize>(px: &[[i32; C]]) -> Option<[i32; C]> {
    let n = px.len() as i32;
    (n > 0).then(|| std::array::from_fn(|c| (px.iter().map(|v| v[c]).sum::<i32>() + n / 2) / n))
}

/// The predicted pixels of a block of a part: of the next smaller mipmap, and of those decoded
/// around (the column to its left and the row above).
struct Around<const C: usize> {
    coarse: Option<[[i32; C]; 16]>,
    left: Option<[[i32; C]; 4]>,
    top: Option<[[i32; C]; 4]>,
}

impl<const C: usize> Around<C> {
    fn of<T: crate::plane::Px>(plane: &Plane<T, C>, coarse: Option<&Plane<T, C>>, bx: usize, by: usize) -> Self {
        let get = |p: &Plane<T, C>, x: usize, y: usize| p.at(x, y).map(|v| v.get());
        Around {
            coarse: coarse.map(|c| std::array::from_fn(|p| get(c, bx * 4 + p % 4, by * 4 + p / 4))),
            left: (bx > 0).then(|| std::array::from_fn(|k| get(plane, bx * 4 - 1, by * 4 + k))),
            top: (by > 0).then(|| std::array::from_fn(|k| get(plane, bx * 4 + k, by * 4 - 1))),
        }
    }

    /// The source of predictions: the smaller mipmap (0), the pixels around (1), none (2).
    fn source(&self) -> usize {
        match (&self.coarse, &self.left, &self.top) {
            (Some(_), _, _) => 0,
            (None, None, None) => 2,
            _ => 1,
        }
    }

    /// The predicted pixels of the block's pixels `of` (row by row): the smaller mipmap's, else
    /// the pixels around that touch them (or all of them where none do); and the predicted pixel
    /// (x, y) alone, as near as there is.
    fn of_pixels(&self, of: impl Fn(usize) -> bool, out: &mut [[i32; C]; 16]) -> usize {
        let mut n = 0;
        if let Some(c) = &self.coarse {
            for p in 0..16 {
                if of(p) {
                    out[n] = c[p];
                    n += 1;
                }
            }
            return n;
        }
        for p in 0..16 {
            if !of(p) {
                continue;
            }
            if let Some(l) = &self.left
                && p % 4 == 0
            {
                out[n] = l[p / 4];
                n += 1;
            }
            if let Some(t) = &self.top
                && p < 4
            {
                out[n] = t[p];
                n += 1;
            }
        }
        if n == 0 {
            for v in self.left.iter().chain(&self.top).flatten() {
                out[n] = *v;
                n += 1;
            }
        }
        n
    }

    fn at(&self, x: usize, y: usize) -> Option<[i32; C]> {
        if let Some(c) = &self.coarse {
            return Some(c[y * 4 + x]);
        }
        match (&self.left, &self.top) {
            (Some(l), Some(t)) => Some(if x <= y { l[y] } else { t[x] }),
            (Some(l), None) => Some(l[y]),
            (None, Some(t)) => Some(t[x]),
            (None, None) => None,
        }
    }
}

/// Contexts of the residuals of an RGB block's colours: the field kind (the first base of
/// individual, differential, T or H, their second, a planar colour: 10), the source (3), the
/// channel (3), the residual before (4).
const RGB_RES: usize = 10 * 3 * 3 * 4;
/// Of the indices of an individual or differential block: the kind (opaque, punch-through of no
/// opacity), the codeword's class (3), the buckets of the two predictions (9 each); of a T or H
/// block: the mode (2), the kind, the paint colours nearest the two predictions (6 each).
const RGB_IDX: usize = 2 * 3 * 9 * 9 * 4;
const PAINT_IDX: usize = 2 * 2 * 6 * 6 * 4;

/// The models of the RGB blocks of a texture.
#[derive(Clone)]
pub struct Rgb {
    copies: Copies<8>,
    mode: Vec<Bit>,
    opaque: Vec<Bit>,
    flip: Vec<Bit>,
    res: Vec<Bit>,
    res_next: Vec<Bit>,
    delta: Vec<Bit>,
    cw: Vec<Bit>,
    dist: Vec<Bit>,
    over: Vec<Bit>,
    idx: Vec<Bit>,
    paint: Vec<Bit>,
}

/// What a block of an RGB part was, for the contexts of the blocks after: its word, its mode, its
/// choice, its flip and codewords.
#[derive(Clone, Copy, Default)]
struct Seen {
    w: u64,
    mode: u8,
    choice: u8,
}

impl Rgb {
    const SIZES: [usize; 11] = [
        36 * 8,
        2 * 6,
        9 * 3 * 2,
        RGB_RES * 16,
        RGB_RES * 16,
        3 * 4 * 8,
        2 * 4 * 9 * 8,
        2 * 4 * 8,
        3 * 6 * 2,
        RGB_IDX,
        PAINT_IDX,
    ];

    fn new() -> Rgb {
        let v = |n| vec![Bit::default(); n];
        let [mode, opaque, flip, res, res_next, delta, cw, dist, over, idx, paint] = Self::SIZES;
        Rgb {
            copies: Copies::new(),
            mode: v(mode),
            opaque: v(opaque),
            flip: v(flip),
            res: v(res),
            res_next: v(res_next),
            delta: v(delta),
            cw: v(cw),
            dist: v(dist),
            over: v(over),
            idx: v(idx),
            paint: v(paint),
        }
    }

    fn plan() -> u64 {
        let bits: u64 = Self::SIZES.iter().map(|&n| aqr_api::stack::block((n * size_of::<Bit>()) as u64)).sum();
        bits + Copies::<8>::plan()
    }

    fn copy_from(&mut self, o: &Rgb) {
        self.copies.copy_from(&o.copies);
        self.mode.clone_from(&o.mode);
        self.opaque.clone_from(&o.opaque);
        self.flip.clone_from(&o.flip);
        self.res.clone_from(&o.res);
        self.res_next.clone_from(&o.res_next);
        self.delta.clone_from(&o.delta);
        self.cw.clone_from(&o.cw);
        self.dist.clone_from(&o.dist);
        self.over.clone_from(&o.over);
        self.idx.clone_from(&o.idx);
        self.paint.clone_from(&o.paint);
    }

    /// The head of an RGB word: all but its indices (all of it of a planar block).
    fn head(w: u64, f: Format) -> u128 {
        match etc::mode_of(w, f) {
            PLANAR => w as u128,
            _ => (w >> 32) as u128 | 1 << 64,
        }
    }

    /// The colour channels of field kind `kind`, predicted as `pred` (8 bits), coded into `w`'s
    /// fields `fs` (R, G, B) of `bits` bits each (when encoding, `want`'s).
    #[allow(clippy::too_many_arguments)]
    fn colour(
        &mut self,
        s: &mut impl Side,
        want: u64,
        w: &mut u64,
        fs: [etc::Field; 3],
        bits: [u32; 3],
        pred: [i32; 3],
        (kind, src): (usize, usize),
    ) {
        let mut rg = 0;
        for (slot, c) in [1usize, 0, 2].into_iter().enumerate() {
            let n = bits[c];
            let mut p = (pred[c].clamp(0, 255) * ((1 << n) - 1) + 127) / 255;
            if c != 1 {
                p += rg;
            }
            let before = if c == 1 { 0 } else { class(rg) };
            let ctx = (((kind * 3 + src) * 3 + slot) * 4 + before) * 16;
            let v = zz(wrap(etc::get(want, fs[c]) as i32 - p, n));
            let v = s.token(&mut self.res[ctx..ctx + 16], &mut self.res_next[ctx..ctx + 16], v, Cat::Endpoints);
            let r = wrap(unzz(v), n);
            *w = etc::set(*w, fs[c], ((p + r) & ((1 << n) - 1)) as u32);
            if c == 1 {
                rg = r;
            }
        }
    }

    /// Codes the RGB blocks of a surface of `bw` × `bh` blocks, the 8 bytes of each at the start
    /// of each `stride` of `blocks` (given when encoding, filled in when decoding), the plane of the
    /// surface before upsampled being `prev`; gives its plane (in `px`).
    #[allow(clippy::too_many_arguments)]
    fn surface(
        &mut self,
        s: &mut impl Side,
        f: Format,
        blocks: &mut [u8],
        stride: usize,
        (bw, bh): (usize, usize),
        prev: Option<&Plane<u8, 4>>,
        (px, seen): (Vec<[u8; 4]>, &mut Vec<Seen>),
    ) -> Result<Plane<u8, 4>> {
        let mut plane = Plane::new_in(px, bw * 4, bh * 4);
        seen.clear();
        let enc = s.encoding();
        for by in 0..bh {
            for bx in 0..bw {
                let i = by * bw + bx;
                let at = &mut blocks[i * stride..][..8];
                let want = if enc { u64::from_be_bytes((*at).try_into().unwrap()) } else { 0 };
                let left = (bx > 0).then(|| seen[i - 1]);
                let up = (by > 0).then(|| seen[i - bw]);
                let x = self.block(s, f, want, (left, up), &mut plane, prev, (bx, by))?;
                at.copy_from_slice(&x.w.to_be_bytes());
                self.copies.used(Self::head(x.w, f), x.w.to_be_bytes());
                seen.push(x);
            }
        }
        Ok(plane)
    }

    #[allow(clippy::too_many_arguments)]
    fn block(
        &mut self,
        s: &mut impl Side,
        f: Format,
        want: u64,
        (left, up): (Option<Seen>, Option<Seen>),
        plane: &mut Plane<u8, 4>,
        coarse: Option<&Plane<u8, 4>>,
        (bx, by): (usize, usize),
    ) -> Result<Seen> {
        let (lc, uc) = (left.map_or(NONE, |l| l.choice), up.map_or(NONE, |u| u.choice));
        let (lw, uw) = (left.map(|l| l.w.to_be_bytes()), up.map(|u| u.w.to_be_bytes()));
        let head = |b: &[u8; 8]| Self::head(u64::from_be_bytes(*b), f);
        match self.copies.code(s, &want.to_be_bytes(), lw.as_ref(), uw.as_ref(), lc, uc, head)? {
            Got::Block(b, choice) => {
                let w = u64::from_be_bytes(b);
                set(plane, &etc::pixels(w, f), bx, by);
                return Ok(Seen { w, mode: etc::mode_of(w, f), choice });
            }
            Got::Head(b, choice) => {
                let w = u64::from_be_bytes(b);
                let mode = etc::mode_of(w, f);
                if mode == PLANAR {
                    set(plane, &etc::pixels(w, f), bx, by);
                    return Ok(Seen { w, mode, choice });
                }
                let w = self.indices(s, f, w & !0xffff_ffff, want, mode, plane, coarse, (bx, by));
                return Ok(Seen { w, mode, choice });
            }
            Got::New => {}
        }
        let a = Around::of(plane, coarse, bx, by);
        let src = a.source();
        // the mode, by those of the blocks around
        let wm = etc::mode_of(want, f);
        let (ml, mu) = (left.map_or(5, |l| l.mode as usize), up.map_or(5, |u| u.mode as usize));
        let mode = match f {
            Format::Etc1 => s.bit(&mut self.mode[ml * 6 + mu], wm == DIFFERENTIAL, Cat::Mode) as u8,
            _ => {
                let ctx = (ml * 6 + mu) * 8;
                s.tree(&mut self.mode[ctx..ctx + 8], 3, wm as u32, Cat::Mode).min(4) as u8
            }
        };
        let mut w: u64 = 0;
        // bit 33: the mode's (individual 0, else 1), or punch-through's opacity
        let bit33 = match etc::punch(f) {
            true => {
                let lo = left.map_or(2, |l| (l.w >> 33 & 1) as usize);
                s.bit(&mut self.opaque[mode.min(1) as usize * 3 + lo], want >> 33 & 1 != 0, Cat::Mode)
            }
            false => mode != INDIVIDUAL,
        };
        w |= (bit33 as u64) << 33;
        let fs = FIELDS[mode as usize];
        match mode {
            INDIVIDUAL | DIFFERENTIAL => {
                // the flip by those around and by which way the predicted pixels split better
                let wf = etc::get(want, fs[8]) != 0;
                let fl = |b: Option<Seen>| match b {
                    Some(b) if b.mode <= DIFFERENTIAL => etc::get(b.w, FIELDS[b.mode as usize][8]) as usize,
                    _ => 2,
                };
                let better = split(&a);
                let flip = s.bit(&mut self.flip[(fl(left) * 3 + fl(up)) * 3 + better], wf, Cat::Mode);
                w = etc::set(w, fs[8], flip as u32);
                // each subblock's base by the mean of its predicted pixels
                let mut sub = [[0i32; 3]; 2];
                let mut spread = [0usize; 2];
                for k in 0..2 {
                    let mut px = [[0i32; 4]; 16];
                    let n = a.of_pixels(|p| etc::subblock(p, flip) == k, &mut px);
                    let m = mean(&px[..n]).unwrap_or([128; 4]);
                    sub[k] = [m[0], m[1], m[2]];
                    spread[k] = spread_class(&px[..n]);
                }
                let mi = mode as usize;
                match mode {
                    INDIVIDUAL => {
                        for k in 0..2 {
                            self.colour(s, want, &mut w, [fs[k], fs[2 + k], fs[4 + k]], [4; 3], sub[k], (k * 5, src));
                        }
                    }
                    _ => {
                        self.colour(s, want, &mut w, [fs[0], fs[2], fs[4]], [5; 3], sub[0], (1, src));
                        // the second base as the difference from the first, of 3 bits
                        for (slot, c) in [1usize, 0, 2].into_iter().enumerate() {
                            let base = etc::get(w, fs[2 * c]) as i32;
                            let q = (sub[1][c].clamp(0, 255) * 31 + 127) / 255;
                            let p = (q - base).clamp(-4, 3);
                            let ctx = (slot * 4 + (p + 4) as usize / 2) * 8;
                            let v = (etc::get(want, fs[2 * c + 1]) as i32 - p) & 7;
                            let v = s.tree(&mut self.delta[ctx..ctx + 8], 3, v as u32, Cat::Endpoints) as i32;
                            w = etc::set(w, fs[2 * c + 1], ((p + v) & 7) as u32);
                        }
                    }
                }
                // the codewords, by the spread of the predicted pixels (the second, by the first's)
                for k in 0..2 {
                    let before = if k == 0 { 8 } else { etc::get(w, fs[6]) as usize };
                    let ctx = (((mi * 4 + spread[k]) * 9) + before) * 8;
                    let v = s.tree(&mut self.cw[ctx..ctx + 8], 3, etc::get(want, fs[6 + k]), Cat::Endpoints);
                    w = etc::set(w, fs[6 + k], v);
                }
            }
            T | H => {
                let mut px = [[0i32; 4]; 16];
                let n = a.of_pixels(|_| true, &mut px);
                let m = mean(&px[..n]).unwrap_or([128; 4]);
                let k0 = if mode == T { 2 } else { 3 };
                self.colour(s, want, &mut w, [fs[0], fs[1], fs[2]], [4; 3], [m[0], m[1], m[2]], (k0, src));
                let c1 = [0, 1, 2].map(|c| etc::ext(etc::get(w, fs[c]) as i32, 4));
                self.colour(s, want, &mut w, [fs[3], fs[4], fs[5]], [4; 3], c1, (k0 + 5, src));
                let rest = &fs[6..];
                for (j, &fl) in rest.iter().enumerate() {
                    let n = etc::bits(fl);
                    let ctx = ((mode - T) as usize * 4 + j) * 8;
                    let v = s.tree(&mut self.dist[ctx..ctx + 8], n, etc::get(want, fl), Cat::Endpoints);
                    w = etc::set(w, fl, v);
                }
            }
            _ => {
                // planar: O, H and V by the predicted pixels at (0, 0), (3, 0) and (0, 3)
                let o = a.at(0, 0).map_or([128; 3], |v| [v[0], v[1], v[2]]);
                let ext = |q: Option<[i32; 4]>| q.map_or(o, |v| std::array::from_fn(|c| o[c] + (v[c] - o[c]) * 4 / 3));
                let (hp, vp) = (ext(a.at(3, 0)), ext(a.at(0, 3)));
                for (k, pred) in [o, hp, vp].into_iter().enumerate() {
                    self.colour(s, want, &mut w, [fs[3 * k], fs[3 * k + 1], fs[3 * k + 2]], [6, 7, 6], pred, (4, src));
                }
            }
        }
        // the bits left over, by the mode and the bit
        let over = !etc::used(mode);
        let mut j = 0;
        for b in (0..64).rev() {
            if over >> b & 1 != 0 {
                let ctx = ((mode as usize).saturating_sub(2).min(2) * 6 + j.min(5)) * 2;
                let ctx = ctx + (w >> 63 & 1) as usize;
                let v = s.bit(&mut self.over[ctx], want >> b & 1 != 0, Cat::Mode);
                w |= (v as u64) << b;
                j += 1;
            }
        }
        if mode == PLANAR {
            set(plane, &etc::pixels(w, f), bx, by);
            return Ok(Seen { w, mode, choice: NEW });
        }
        let w = self.indices(s, f, w, want, mode, plane, coarse, (bx, by));
        Ok(Seen { w, mode, choice: NEW })
    }

    /// The indices of block `w` (all but its indices known; when encoding, `want`'s), its pixels
    /// set in `plane` as they decode.
    #[allow(clippy::too_many_arguments)]
    fn indices(
        &mut self,
        s: &mut impl Side,
        f: Format,
        mut w: u64,
        want: u64,
        mode: u8,
        plane: &mut Plane<u8, 4>,
        coarse: Option<&Plane<u8, 4>>,
        (bx, by): (usize, usize),
    ) -> u64 {
        let opaque = !etc::punch(f) || w >> 33 & 1 != 0;
        let kind = (!opaque) as usize;
        // of an individual or differential block, the ladder of each subblock: the sums of the
        // channels of its levels (−b, −a or 0, +a or 0, +b), the indices of those levels
        let (bases, cws, flip) = if mode <= DIFFERENTIAL { etc::bases(w, mode) } else { ([[0; 3]; 2], [0; 2], false) };
        let paints = if mode > DIFFERENTIAL { etc::paints(w, mode) } else { [[0; 3]; 4] };
        const LEVEL_INDEX: [u32; 4] = [3, 2, 0, 1];
        let ladders: [[i32; 4]; 2] = std::array::from_fn(|k| {
            std::array::from_fn(|l| {
                let c = etc::modified(bases[k], cws[k], LEVEL_INDEX[l], true);
                c[0] as i32 + c[1] as i32 + c[2] as i32
            })
        });
        for p in 0..16 {
            let (x, y) = (bx * 4 + p % 4, by * 4 + p / 4);
            let (q1, q2) = match coarse {
                Some(c) => (Some(c.at(x, y)), plane.causal(x, y)),
                None => ((y > 0).then(|| plane.at(x, y - 1)), (x > 0).then(|| plane.at(x - 1, y))),
            };
            let v = etc::index(want, p);
            let i = match mode {
                INDIVIDUAL | DIFFERENTIAL => {
                    let k = etc::subblock(p, flip);
                    // (a transparent prediction is a bucket of its own: 8; none, 8 too)
                    let b = |q: Option<[u8; 4]>| match q {
                        Some(q) if q[3] >= 128 => rung(&ladders[k], q[0] as i32 + q[1] as i32 + q[2] as i32),
                        _ => 8,
                    };
                    let contrast = (cws[k] / 3).min(2);
                    let ctx = (((kind * 3 + contrast) * 9 + b(q1)) * 9 + b(q2)) * 4;
                    let lv = s.tree(
                        &mut self.idx[ctx..ctx + 4],
                        2,
                        LEVEL_INDEX.iter().position(|&i| i == v).unwrap_or(0) as u32,
                        Cat::Indices,
                    );
                    LEVEL_INDEX[lv as usize]
                }
                _ => {
                    // the paint colour nearest each prediction (4: transparent, 5: none)
                    let near = |q: Option<[u8; 4]>| match q {
                        Some(q) if q[3] < 128 => 4,
                        Some(q) => (0..4)
                            .min_by_key(|&j| (0..3).map(|c| (q[c] as i32 - paints[j][c]).pow(2)).sum::<i32>())
                            .unwrap(),
                        None => 5,
                    };
                    let ctx = ((((mode - T) as usize * 2 + kind) * 6 + near(q1)) * 6 + near(q2)) * 4;
                    s.tree(&mut self.paint[ctx..ctx + 4], 2, v, Cat::Indices)
                }
            };
            let k = etc::column(p);
            w |= ((i >> 1) as u64) << (16 + k) | ((i & 1) as u64) << k;
            let px = match mode {
                INDIVIDUAL | DIFFERENTIAL => {
                    let s = etc::subblock(p, flip);
                    etc::modified(bases[s], cws[s], i, opaque)
                }
                _ if !opaque && i == 2 => [0; 4],
                _ => {
                    let c = paints[i as usize];
                    [c[0] as u8, c[1] as u8, c[2] as u8, 255]
                }
            };
            plane.set(x, y, px);
        }
        w
    }
}

/// The class of the spread of pixels `px`: of their channels' sum, the range (none, up to 24, 96,
/// more).
fn spread_class(px: &[[i32; 4]]) -> usize {
    let sums = px.iter().map(|v| v[0] + v[1] + v[2]);
    let (lo, hi) = sums.clone().fold((i32::MAX, i32::MIN), |(a, b), v| (a.min(v), b.max(v)));
    match hi.saturating_sub(lo) {
        i32::MIN..=24 => 0,
        25..=96 => 1,
        97..=300 => 2,
        _ => 3,
    }
}

/// Which way the predicted pixels of a block split into two of less spread: 0 left and right, 1
/// top and bottom, 2 neither (no smaller mipmap).
fn split(a: &Around<4>) -> usize {
    let Some(c) = &a.coarse else { return 2 };
    let cost = |flip: bool| -> i32 {
        (0..2)
            .map(|k| {
                let of = |p: usize| etc::subblock(p, flip) == k;
                let sum = |p: usize| c[p][0] + c[p][1] + c[p][2];
                let m = (0..16).filter(|&p| of(p)).map(sum).sum::<i32>() / 8;
                (0..16).filter(|&p| of(p)).map(|p| (sum(p) - m).abs()).sum::<i32>()
            })
            .sum()
    };
    (cost(true) < cost(false)) as usize
}

/// The pixels `px` of a block set in `plane` at (`bx`, `by`).
fn set<T: crate::plane::Px, const C: usize>(plane: &mut Plane<T, C>, px: &[[T; C]; 16], bx: usize, by: usize) {
    for p in 0..16 {
        plane.set(bx * 4 + p % 4, by * 4 + p / 4, px[p]);
    }
}

/// The level of each EAC index (by its modifier, from the least), and the index of each level.
const EAC_LEVEL_INDEX: [u32; 8] = [3, 2, 1, 0, 4, 5, 6, 7];

/// Contexts of EAC indices: the contrast (3), the buckets of the two predictions (17 each).
const EAC_IDX: usize = 3 * 17 * 17 * 8;

/// The models of the EAC blocks of a texture's channel.
#[derive(Clone)]
pub struct Values {
    copies: Copies<8>,
    res: Vec<Bit>,
    res_next: Vec<Bit>,
    table: Vec<Bit>,
    mult: Vec<Bit>,
    idx: Vec<Bit>,
    flat: Vec<Bit>,
}

impl Values {
    const SIZES: [usize; 6] = [3 * 16, 3 * 16, 17 * 16, 9 * 16, EAC_IDX, 9 * 9 * 8];

    fn new() -> Values {
        let v = |n| vec![Bit::default(); n];
        let [res, res_next, table, mult, idx, flat] = Self::SIZES;
        Values {
            copies: Copies::new(),
            res: v(res),
            res_next: v(res_next),
            table: v(table),
            mult: v(mult),
            idx: v(idx),
            flat: v(flat),
        }
    }

    fn plan() -> u64 {
        let bits: u64 = Self::SIZES.iter().map(|&n| aqr_api::stack::block((n * size_of::<Bit>()) as u64)).sum();
        bits + Copies::<8>::plan()
    }

    fn copy_from(&mut self, o: &Values) {
        self.copies.copy_from(&o.copies);
        self.res.clone_from(&o.res);
        self.res_next.clone_from(&o.res_next);
        self.table.clone_from(&o.table);
        self.mult.clone_from(&o.mult);
        self.idx.clone_from(&o.idx);
        self.flat.clone_from(&o.flat);
    }

    /// As [`Rgb::surface`], of EAC blocks of values `e`.
    #[allow(clippy::too_many_arguments)]
    fn surface(
        &mut self,
        s: &mut impl Side,
        e: Eac,
        blocks: &mut [u8],
        stride: usize,
        (bw, bh): (usize, usize),
        prev: Option<&Plane<u16, 1>>,
        (px, seen): (Vec<[u16; 1]>, &mut Vec<Seen>),
    ) -> Result<Plane<u16, 1>> {
        let mut plane = Plane::new_in(px, bw * 4, bh * 4);
        seen.clear();
        let enc = s.encoding();
        let head = |b: &[u8; 8]| (u64::from_be_bytes(*b) >> 48) as u128;
        for by in 0..bh {
            for bx in 0..bw {
                let i = by * bw + bx;
                let at = &mut blocks[i * stride..][..8];
                let want = if enc { u64::from_be_bytes((*at).try_into().unwrap()) } else { 0 };
                let left = (bx > 0).then(|| seen[i - 1]);
                let up = (by > 0).then(|| seen[i - bw]);
                let (lc, uc) = (left.map_or(NONE, |l| l.choice), up.map_or(NONE, |u| u.choice));
                let (lw, uw) = (left.map(|l| l.w.to_be_bytes()), up.map(|u| u.w.to_be_bytes()));
                let (w, choice) =
                    match self.copies.code(s, &want.to_be_bytes(), lw.as_ref(), uw.as_ref(), lc, uc, head)? {
                        Got::Block(b, choice) => {
                            let w = u64::from_be_bytes(b);
                            set(&mut plane, &e.pixels(w).map(|v| [v]), bx, by);
                            (w, choice)
                        }
                        Got::Head(b, choice) => {
                            let w = u64::from_be_bytes(b) & !((1 << 48) - 1);
                            (self.indices(s, e, w, want, &mut plane, prev, (bx, by)), choice)
                        }
                        Got::New => {
                            let w = self.head(s, e, want, left.map(|l| l.w), &plane, prev, (bx, by));
                            (self.indices(s, e, w, want, &mut plane, prev, (bx, by)), NEW)
                        }
                    };
                at.copy_from_slice(&w.to_be_bytes());
                self.copies.used(head(&w.to_be_bytes()), w.to_be_bytes());
                seen.push(Seen { w, mode: 0, choice });
            }
        }
        Ok(plane)
    }

    /// A new head of an EAC block (when encoding, `want`'s): its base by the mean of the predicted
    /// values, its table by the left block's, its multiplier by the spread of the predicted values
    /// against the table's.
    #[allow(clippy::too_many_arguments)]
    fn head(
        &mut self,
        s: &mut impl Side,
        e: Eac,
        want: u64,
        left: Option<u64>,
        plane: &Plane<u16, 1>,
        coarse: Option<&Plane<u16, 1>>,
        (bx, by): (usize, usize),
    ) -> u64 {
        let a = Around::of(plane, coarse, bx, by);
        let src = a.source();
        let mut px = [[0i32; 1]; 16];
        let n = a.of_pixels(|_| true, &mut px);
        let vals: Vec<i32> = px[..n].iter().map(|v| v[0]).collect();
        let m = mean(&px[..n]).map_or(
            match e {
                Eac::Alpha => 128,
                _ => 1024,
            },
            |v| v[0],
        );
        let range = vals.iter().max().zip(vals.iter().min()).map(|(a, b)| a - b);
        // the base: of alpha the value, of R11 the value / 8 (signed: of −1023 to 1023)
        let pred = match e {
            Eac::Alpha => m,
            Eac::R11 => (m - 4) >> 3,
            Eac::R11Signed => ((m - 1024) >> 3) & 255,
        };
        let ctx = src * 16;
        let v = zz(wrap((want >> 56) as i32 - pred, 8));
        let v = s.token(&mut self.res[ctx..ctx + 16], &mut self.res_next[ctx..ctx + 16], v, Cat::Endpoints);
        let base = ((pred + wrap(unzz(v), 8)) & 255) as u64;
        let lt = left.map_or(16, |l| (l >> 48 & 15) as usize);
        let t = s.tree(&mut self.table[lt * 16..][..16], 4, (want >> 48 & 15) as u32, Cat::Mode) as u64;
        // the spread as times the table's (of R11, its values in eighths)
        let span = EAC[t as usize][7] - EAC[t as usize][3];
        let unit = if e == Eac::Alpha { 1 } else { 8 };
        let sc = range.map_or(8, |r| ((r + unit * span / 2) / (unit * span)).min(7) as usize);
        let mult = s.tree(&mut self.mult[sc * 16..][..16], 4, (want >> 52 & 15) as u32, Cat::Mode) as u64;
        base << 56 | mult << 52 | t << 48
    }

    /// The indices of EAC block `w` (its head known; when encoding, `want`'s), its pixels set in
    /// `plane` as they decode.
    #[allow(clippy::too_many_arguments)]
    fn indices(
        &mut self,
        s: &mut impl Side,
        e: Eac,
        mut w: u64,
        want: u64,
        plane: &mut Plane<u16, 1>,
        coarse: Option<&Plane<u16, 1>>,
        (bx, by): (usize, usize),
    ) -> u64 {
        let values = e.values(w);
        let ladder: [i32; 8] = std::array::from_fn(|l| values[EAC_LEVEL_INDEX[l] as usize]);
        let flat = ladder[0] == ladder[7];
        let contrast = match ladder[7] - ladder[0] {
            ..=8 => 0,
            9..=64 => 1,
            _ => 2,
        };
        let mut got = [0u32; 16];
        for p in 0..16 {
            let (x, y) = (bx * 4 + p % 4, by * 4 + p / 4);
            let v = (want >> (45 - 3 * etc::column(p))) as u32 & 7;
            let i = match flat {
                false => {
                    let (q1, q2) = match coarse {
                        Some(c) => (Some(c.at(x, y)), plane.causal(x, y)),
                        None => ((y > 0).then(|| plane.at(x, y - 1)), (x > 0).then(|| plane.at(x - 1, y))),
                    };
                    let b = |q: Option<[u16; 1]>| q.map_or(16, |q| rung(&ladder, q[0] as i32));
                    let ctx = ((contrast * 17 + b(q1)) * 17 + b(q2)) * 8;
                    let lv = EAC_LEVEL_INDEX.iter().position(|&i| i == v).unwrap_or(0) as u32;
                    EAC_LEVEL_INDEX[s.tree(&mut self.idx[ctx..ctx + 8], 3, lv, Cat::Indices) as usize]
                }
                true => {
                    let left = if p % 4 > 0 { got[p - 1] as usize + 1 } else { 0 };
                    let above = if p >= 4 { got[p - 4] as usize + 1 } else { 0 };
                    let ctx = (left * 9 + above) * 8;
                    s.tree(&mut self.flat[ctx..ctx + 8], 3, v, Cat::Indices)
                }
            };
            got[p] = i;
            w |= (i as u64) << (45 - 3 * etc::column(p));
            plane.set(x, y, [values[i as usize] as u16]);
        }
        w
    }
}

use crate::etc::EAC;

/// The parts of a format: RGB, and the first and second EAC parts (and what their values are).
fn parts(f: Format) -> (bool, Option<Eac>, Option<Eac>) {
    match f {
        Format::Etc1 | Format::Etc2 | Format::Etc2P => (true, None, None),
        Format::Etc2A => (true, Some(Eac::Alpha), None),
        Format::Eac { rg, signed } => {
            let e = if signed { Eac::R11Signed } else { Eac::R11 };
            (false, Some(e), rg.then_some(e))
        }
        _ => unreachable!("not of ETC"),
    }
}

/// The models of a texture, or of a stripe.
#[derive(Clone)]
pub struct Models {
    rgb: Rgb,
    a: Values,
    b: Values,
}

/// The planes of a surface: of RGBA, and of the first and second EAC parts' values.
#[derive(Default)]
pub struct Planes {
    c: Option<Plane<u8, 4>>,
    a: Option<Plane<u16, 1>>,
    b: Option<Plane<u16, 1>>,
}

#[derive(Default)]
pub struct Bufs {
    c: Vec<[u8; 4]>,
    a: Vec<[u16; 1]>,
    b: Vec<[u16; 1]>,
}

/// Buffers of a surface's coding: its blocks seen, and the upsamplings'.
pub struct Work {
    seen: Vec<Seen>,
    up8: Up<u16>,
    up16: Up<u32>,
}

impl Family for Etc {
    const GROUP: Group = Group::Etc;

    #[inline(always)]
    fn shape(f: Format) -> Shape {
        Shape { fw: 4, fh: 4, least: 1, bytes: f.block_bytes() }
    }

    type Models = Models;
    type Planes = Planes;
    type Bufs = Bufs;
    type Work = Work;

    fn models(_: Format) -> Models {
        Models { rgb: Rgb::new(), a: Values::new(), b: Values::new() }
    }

    fn models_plan(_: Format) -> u64 {
        Rgb::plan() + 2 * Values::plan()
    }

    fn copy_models(m: &mut Models, o: &Models) {
        m.rgb.copy_from(&o.rgb);
        m.a.copy_from(&o.a);
        m.b.copy_from(&o.b);
    }

    fn bufs(f: Format, px: usize) -> Bufs {
        let (c, a, b) = parts(f);
        let n = |k: bool| if k { px } else { 0 };
        Bufs {
            c: Vec::with_capacity(n(c)),
            a: Vec::with_capacity(n(a.is_some())),
            b: Vec::with_capacity(n(b.is_some())),
        }
    }

    fn bufs_plan(f: Format, px: usize) -> u64 {
        use aqr_api::stack::block;
        let (c, a, b) = parts(f);
        let px = px as u64;
        block(c as u64 * 4 * px) + block(a.is_some() as u64 * 2 * px) + block(b.is_some() as u64 * 2 * px)
    }

    fn give(bufs: &mut Bufs, p: Planes) {
        if let Some(c) = p.c {
            bufs.c = c.into_px();
        }
        if let Some(a) = p.a {
            bufs.a = a.into_px();
        }
        if let Some(b) = p.b {
            bufs.b = b.into_px();
        }
    }

    fn is_empty(p: &Planes) -> bool {
        p.c.is_none() && p.a.is_none() && p.b.is_none()
    }

    fn work(f: Format, blocks: usize, cols: usize, across: usize) -> Work {
        // (the upsampling of the colours takes 4 values a pixel, of the EAC parts' 1)
        let (c, a, _) = parts(f);
        let (a8, a16) = (if c { across } else { 0 }, if a.is_some() { across / Self::across_values(f) } else { 0 });
        Work { seen: Vec::with_capacity(blocks), up8: Up::with_capacity(cols, a8), up16: Up::with_capacity(cols, a16) }
    }

    fn work_plan(f: Format, blocks: usize, cols: usize, across: usize) -> u64 {
        use aqr_api::stack::block;
        let (c, a, _) = parts(f);
        let (a8, a16) = (if c { across } else { 0 }, if a.is_some() { across / Self::across_values(f) } else { 0 });
        block((blocks * size_of::<Seen>()) as u64) + Up::<u16>::plan(cols, a8) + Up::<u32>::plan(cols, a16)
    }

    fn across_values(f: Format) -> usize {
        if parts(f).0 { 4 } else { 1 }
    }

    fn planes_of(f: Format, b: &[u8], (bw, bh): (usize, usize), p: &mut Planes) {
        let (c, a, pb) = parts(f);
        let bb = f.block_bytes();
        let word = |blk: &[u8], at: usize| u64::from_be_bytes(blk[at..at + 8].try_into().unwrap());
        // (the RGB part of RGBA blocks is their second half)
        let rgb_at = if a.is_some() { 8 } else { 0 };
        let y0c = p.c.as_mut().filter(|_| c).map(|pl| pl.grow(bh * 4));
        let y0a = p.a.as_mut().map(|pl| pl.grow(bh * 4));
        let y0b = p.b.as_mut().map(|pl| pl.grow(bh * 4));
        for (i, blk) in b.chunks(bb).take(bw * bh).enumerate() {
            let (x0, y) = (i % bw * 4, i / bw * 4);
            if let (Some(pl), Some(y0)) = (p.c.as_mut(), y0c) {
                let rf = if f == Format::Etc2A { Format::Etc2 } else { f };
                let px = etc::pixels(word(blk, rgb_at), rf);
                for q in 0..16 {
                    pl.set(x0 + q % 4, y0 + y + q / 4, px[q]);
                }
            }
            for (e, plane, y0, at) in [(a, p.a.as_mut(), y0a, 0), (pb, p.b.as_mut(), y0b, 8)] {
                if let (Some(e), Some(pl), Some(y0)) = (e, plane, y0) {
                    let px = e.pixels(word(blk, at));
                    for q in 0..16 {
                        pl.set(x0 + q % 4, y0 + y + q / 4, [px[q]]);
                    }
                }
            }
        }
    }

    fn upsampled(
        p: &Planes,
        _: Format,
        w: usize,
        rows: std::ops::Range<usize>,
        bufs: &mut Bufs,
        work: &mut Work,
    ) -> Planes {
        Planes {
            c: p.c.as_ref().map(|p| p.upsampled::<4>(w, rows.clone(), std::mem::take(&mut bufs.c), &mut work.up8)),
            a: p.a.as_ref().map(|p| p.upsampled::<4>(w, rows.clone(), std::mem::take(&mut bufs.a), &mut work.up16)),
            b: p.b.as_ref().map(|p| p.upsampled::<4>(w, rows, std::mem::take(&mut bufs.b), &mut work.up16)),
        }
    }

    fn stripe(
        s: &mut impl Side,
        m: &mut Models,
        f: Format,
        b: &mut [u8],
        size: (usize, usize),
        up: &Planes,
        bufs: &mut Bufs,
        work: &mut Work,
    ) -> Result<Planes> {
        let (c, a, pb) = parts(f);
        let bb = f.block_bytes();
        let mut out = Planes::default();
        if let Some(e) = a {
            out.a =
                Some(m.a.surface(s, e, b, bb, size, up.a.as_ref(), (std::mem::take(&mut bufs.a), &mut work.seen))?);
        }
        if let Some(e) = pb {
            let r = &mut b[8..];
            out.b =
                Some(m.b.surface(s, e, r, bb, size, up.b.as_ref(), (std::mem::take(&mut bufs.b), &mut work.seen))?);
        }
        if c {
            let (rgb, rf) = match f {
                Format::Etc2A => (&mut b[8..], Format::Etc2),
                _ => (&mut b[..], f),
            };
            out.c = Some(m.rgb.surface(
                s,
                rf,
                rgb,
                bb,
                size,
                up.c.as_ref(),
                (std::mem::take(&mut bufs.c), &mut work.seen),
            )?);
        }
        Ok(out)
    }
}
