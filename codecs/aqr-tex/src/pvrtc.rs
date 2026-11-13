//! PVRTC1 (4 and 2 bits a pixel) as a family of the walk ([`crate::texture`]; FORMAT.md,
//! "`pvrtc-recons`"). A word's colours A and B are samples of two images of low resolution,
//! each pixel's colour between the images blended from the 4 words around it (wrapping at the
//! sides), by its modulation. So a surface is coded in two passes, of no stripes: the colours of
//! every word (as the word to its left or above, its colours as theirs or a word's before, or
//! new: predicted from the words around), then the modulation of the words not copied whole,
//! each pixel's as a level on the line between the images there, by where the pixels around
//! predict it. The walk takes the words row by row; a surface's are in reflected Morton order.
//! One plane of RGBA.

use aqr_common::arith::Bit;

use crate::Result;
use crate::coder::{Cat, Side, bit_models, class, unzz, wrap, zz};
use crate::copies::{Copies, Got, NEW, NONE};
use crate::layout::{Format, Group, Shape};
use crate::plane::{Plane, Up, med};
use crate::texture::Family;

pub struct Pvrtc;

/// The offset of word (x, y) of a surface of `w` × `h` words (powers of two) in reflected Morton
/// order: the bits of x and y interleaved, y's the lower, up to the bits of the lesser side, then
/// the rest of the greater's.
pub fn morton(x: usize, y: usize, w: usize, h: usize) -> usize {
    let least = w.min(h);
    let (mut offset, mut shift, mut mask) = (0, 0, 1);
    while mask < least {
        offset |= ((y & mask) | (x & mask) << 1) << shift;
        shift += 1;
        mask <<= 1;
    }
    offset | ((x | y) >> shift) << (2 * shift)
}

/// A word's colour as ARGB 4555 (alpha of 4 bits, the others of 5): colour A (bits 33 to 47, of
/// `b` false) or B (48 to 63), opaque where its top bit is 1.
#[inline]
pub fn colour(w: u64, b: bool) -> [i32; 4] {
    let v = if b { (w >> 48) as u32 & 0xffff } else { (w >> 32) as u32 & 0xfffe };
    let e3 = |c: u32| (c << 2 | c >> 1) as i32;
    let e4 = |c: u32| (c << 1 | c >> 3) as i32;
    match v >> 15 {
        1 => {
            let (r, g) = (v >> 10 & 31, v >> 5 & 31);
            let blue = if b { (v & 31) as i32 } else { e4(v >> 1 & 15) };
            [15, r as i32, g as i32, blue]
        }
        _ => {
            let a = ((v >> 12 & 7) << 1) as i32;
            let (r, g) = (e4(v >> 8 & 15), e4(v >> 4 & 15));
            let blue = if b { e4(v & 15) } else { e3(v >> 1 & 7) };
            [a, r, g, blue]
        }
    }
}

/// The colours of the images A and B at pixel (x, y) of a surface of `w` × `h` words of footprint
/// (`fw`, 4), `words` row by row: each channel bilinear between the 4 words around (wrapping), as
/// 8 bits (ARGB).
#[inline]
pub fn images(words: &[u64], (w, h): (usize, usize), fw: usize, x: usize, y: usize) -> [[i32; 4]; 2] {
    let (hx, hy) = (fw / 2, 2);
    let xl = (x as isize - hx as isize).div_euclid(fw as isize);
    let yl = (y as isize - hy as isize).div_euclid(4);
    let (xr, yr) = ((x as isize - hx as isize - xl * fw as isize) as i32, (y as isize - hy as isize - yl * 4) as i32);
    let at = |dx: isize, dy: isize| {
        let (wx, wy) = ((xl + dx).rem_euclid(w as isize) as usize, (yl + dy).rem_euclid(h as isize) as usize);
        words[wy * w + wx]
    };
    let (w00, w10, w01, w11) = (at(0, 0), at(1, 0), at(0, 1), at(1, 1));
    let fwi = fw as i32;
    std::array::from_fn(|k| {
        let b = k == 1;
        let (c00, c10, c01, c11) = (colour(w00, b), colour(w10, b), colour(w01, b), colour(w11, b));
        std::array::from_fn(|c| {
            let v =
                c00[c] * (fwi - xr) * (4 - yr) + c10[c] * xr * (4 - yr) + c01[c] * (fwi - xr) * yr + c11[c] * xr * yr;
            match (c, fw) {
                (0, 4) => v + v / 16,
                (_, 4) => v / 2 + v / 64,
                (0, _) => v / 2 + v / 32,
                _ => v / 4 + v / 128,
            }
        })
    })
}

/// The colours of the images A and B at each pixel of the word at (bx, by) (row by row, 16 or
/// 32), as [`images`] tells them one by one: of the colours of the 3 × 3 words around it, each
/// decoded once.
#[inline]
pub fn word_images(words: &[u64], (w, h): (usize, usize), fw: usize, bx: usize, by: usize) -> [[[i32; 4]; 2]; 32] {
    let (xs, ys) = ([(bx + w - 1) % w, bx, (bx + 1) % w], [(by + h - 1) % h, by, (by + 1) % h]);
    let mut cs = [[[0i32; 4]; 2]; 9];
    for (j, &wy) in ys.iter().enumerate() {
        for (i, &wx) in xs.iter().enumerate() {
            let v = words[wy * w + wx];
            cs[j * 3 + i] = [colour(v, false), colour(v, true)];
        }
    }
    let (hx, fwi) = (fw / 2, fw as i32);
    let mut out = [[[0i32; 4]; 2]; 32];
    for ly in 0..4 {
        // (the words above or here, and the pixel's place between them)
        let (dy, yr) = if ly < 2 { (0, ly as i32 + 2) } else { (1, ly as i32 - 2) };
        for lx in 0..fw {
            let (dx, xr) = if lx < hx { (0, (lx + hx) as i32) } else { (1, (lx - hx) as i32) };
            let (c00, c10, c01, c11) =
                (&cs[dy * 3 + dx], &cs[dy * 3 + dx + 1], &cs[(dy + 1) * 3 + dx], &cs[(dy + 1) * 3 + dx + 1]);
            let o = &mut out[ly * fw + lx];
            for k in 0..2 {
                for c in 0..4 {
                    let v = c00[k][c] * (fwi - xr) * (4 - yr)
                        + c10[k][c] * xr * (4 - yr)
                        + c01[k][c] * (fwi - xr) * yr
                        + c11[k][c] * xr * yr;
                    o[k][c] = match (c, fw) {
                        (0, 4) => v + v / 16,
                        (_, 4) => v / 2 + v / 64,
                        (0, _) => v / 2 + v / 32,
                        _ => v / 4 + v / 128,
                    };
                }
            }
        }
    }
    out
}

/// The weights of 2-bit modulation values: of modulation flag 0, and 1 (value 2: punch-through,
/// transparent).
pub const WEIGHTS: [[i32; 4]; 2] = [[0, 3, 5, 8], [0, 4, 4, 8]];

/// The colour of a pixel of images `ab` by weight `wt` (transparent of punch-through).
#[inline(always)]
pub fn blend(ab: &[[i32; 4]; 2], wt: i32, punch: bool) -> [u8; 4] {
    let v: [u8; 4] = std::array::from_fn(|c| ((ab[0][c] * (8 - wt) + ab[1][c] * wt) / 8) as u8);
    // (stored ARGB, the planes RGBA)
    [v[1], v[2], v[3], if punch { 0 } else { v[0] }]
}

/// Where the modulation of pixel (x, y) of a 2-bit word is, of a word of modulation flag 1: the
/// bits of a stored value (the low one at), of 2 bits (1 of pixels (0, 0), and (4, 2) of the
/// horizontal and vertical mode), or none (interpolated).
#[inline(always)]
pub fn stored(x: usize, y: usize, hv: bool) -> Option<(u32, u32)> {
    if (x + y) % 2 == 1 {
        return None;
    }
    let k = (y * 8 + x) / 2;
    let lo = 2 * k as u32;
    match (x, y) {
        (0, 0) => Some((1, 1)),
        (4, 2) if hv => Some((21, 1)),
        _ => Some((lo, 2)),
    }
}

/// The weights of the pixels of word `w` of a surface of 2 bits a pixel, row by row (32), as far
/// as the word tells ([`weight2`] tells them all): of modulation flag 0, 0 or 8 by each bit; of
/// flag 1, the stored values' (one bit repeated as two), the others the mean of the stored ones
/// beside them in the word (of all four, or of the horizontal and vertical mode the two of its
/// flag's way; those off the word left out), none punch-through.
pub fn weights2(w: u64) -> [i32; 32] {
    let mut wt = [0i32; 32];
    if w >> 32 & 1 == 0 {
        for p in 0..32 {
            wt[p] = 8 * (w >> p & 1) as i32;
        }
        return wt;
    }
    let hv = w & 1 != 0;
    let vertical = w >> 20 & 1 != 0;
    let value = |x: usize, y: usize| -> Option<u32> {
        let (lo, n) = stored(x, y, hv)?;
        let v = (w >> lo) as u32 & ((1 << n) - 1);
        Some(if n == 1 { v * 3 } else { v })
    };
    for y in 0..4 {
        for x in 0..8 {
            if let Some(v) = value(x, y) {
                wt[y * 8 + x] = WEIGHTS[0][v as usize];
            }
        }
    }
    for y in 0..4 {
        for x in 0..8 {
            if value(x, y).is_some() {
                continue;
            }
            // (the neighbours of its mode within the word, all stored: their mean)
            let ways: &[(isize, isize)] = match (hv, vertical) {
                (false, _) => &[(-1, 0), (1, 0), (0, -1), (0, 1)],
                (true, true) => &[(0, -1), (0, 1)],
                (true, false) => &[(-1, 0), (1, 0)],
            };
            let (mut sum, mut n) = (0, 0);
            for &(dx, dy) in ways {
                let (nx, ny) = (x as isize + dx, y as isize + dy);
                if (0..8).contains(&nx) && (0..4).contains(&ny) {
                    sum += wt[ny as usize * 8 + nx as usize];
                    n += 1;
                }
            }
            wt[y * 8 + x] = if n > 0 { (sum + n / 2) / n } else { 0 };
        }
    }
    wt
}

/// The weight of pixel (x, y) of a surface of 2 bits a pixel, `words` row by row (of `w` × `h`),
/// and whether it is punch-through (never: as PowerVR's decoders, not the Khronos description, of
/// whose weights of mode 1 they are not): of a word of flag 0, 0 or 8 by its bit; of flag 1, a
/// stored value's of 0, 3, 5, 8 (one bit repeated as two); else the mean of the weights beside it (all four, or of the
/// horizontal and vertical mode the two of its flag's way), from the words they are in (wrapping),
/// a neighbour in a word of flag 0 of weight 0 or 8 by its bit.
pub fn weight2(words: &[u64], (w, h): (usize, usize), x: usize, y: usize) -> (i32, bool) {
    let (sw, sh) = ((w * 8) as isize, (h * 4) as isize);
    let wd = |x: usize, y: usize| words[(y / 4) * w + x / 8];
    let value = |x: isize, y: isize| -> i32 {
        let (x, y) = (x.rem_euclid(sw) as usize, y.rem_euclid(sh) as usize);
        let v = wd(x, y);
        if v >> 32 & 1 == 0 {
            return 8 * (v >> ((y % 4) * 8 + x % 8) & 1) as i32;
        }
        match stored(x % 8, y % 4, v & 1 != 0) {
            Some((lo, n)) => {
                let s = (v >> lo) as u32 & ((1 << n) - 1);
                WEIGHTS[0][if n == 1 { s * 3 } else { s } as usize]
            }
            None => 4,
        }
    };
    let v = wd(x, y);
    if v >> 32 & 1 == 0 {
        return (8 * (v >> ((y % 4) * 8 + x % 8) & 1) as i32, false);
    }
    let hv = v & 1 != 0;
    if let Some((lo, n)) = stored(x % 8, y % 4, hv) {
        let s = (v >> lo) as u32 & ((1 << n) - 1);
        return (WEIGHTS[0][if n == 1 { s * 3 } else { s } as usize], false);
    }
    let (x, y) = (x as isize, y as isize);
    let wt = match (hv, v >> 20 & 1 != 0) {
        (false, _) => (value(x - 1, y) + value(x + 1, y) + value(x, y - 1) + value(x, y + 1) + 2) / 4,
        (true, true) => (value(x, y - 1) + value(x, y + 1) + 1) / 2,
        (true, false) => (value(x - 1, y) + value(x + 1, y) + 1) / 2,
    };
    (wt, false)
}

/// The pixels of a surface of 2 bits a pixel set in `plane` as [`weight2`] tells, where they
/// were set as [`pixels`] tells: those [`weights2`] did not know (of words of modulation flag 1,
/// the ones of no value stored).
pub fn exact2(words: &[u64], size: (usize, usize), plane: &mut Plane<u8, 4>) {
    for by in 0..size.1 {
        for bx in 0..size.0 {
            if words[by * size.0 + bx] >> 32 & 1 == 0 {
                continue;
            }
            let ab = word_images(words, size, 8, bx, by);
            for p in (0..32).filter(|p| (p % 8 + p / 8) % 2 == 1) {
                let (x, y) = (bx * 8 + p % 8, by * 4 + p / 8);
                let (wt, punch) = weight2(words, size, x, y);
                plane.set(x, y, blend(&ab[p], wt, punch));
            }
        }
    }
}

/// The pixels of a word at (bx, by) of `words` (row by row, of `w` × `h` words), row by row (16,
/// or 32 of 2 bits a pixel), of the images there `ab` ([`word_images`]).
pub fn pixels(
    words: &[u64],
    size: (usize, usize),
    two: bool,
    bx: usize,
    by: usize,
    ab: &[[[i32; 4]; 2]; 32],
) -> [[u8; 4]; 32] {
    let fw = if two { 8 } else { 4 };
    let wd = words[by * size.0 + bx];
    let (mut wt, mut punch) = ([0i32; 32], [false; 32]);
    match two {
        true => wt = weights2(wd),
        false => {
            let m = (wd >> 32 & 1) as usize;
            for p in 0..16 {
                let v = (wd >> (2 * p)) as usize & 3;
                (wt[p], punch[p]) = (WEIGHTS[m][v], m == 1 && v == 2);
            }
        }
    }
    let mut out = [[0u8; 4]; 32];
    for p in 0..4 * fw {
        out[p] = blend(&ab[p], wt[p], punch[p]);
    }
    out
}

/// Contexts of a new colour's channels: the colour (A, B), its opacity, the channel (4), the
/// activity of the words around (4), the residual before (4).
const RES: usize = 2 * 2 * 4 * 4 * 4;
/// Contexts of modulation: the kind (4 bits, modulation flag 0 or 1; 2 bits, flag 0 or a stored
/// value), the contrast (3), the buckets of the two predictions (12 each).
const MOD: usize = 4 * 3 * 12 * 12 * 4;

bit_models! {
    /// The models of a texture.
    #[derive(Clone)]
    pub struct Models {
        copies: Copies<8>,
        opaque: 2 * 3 * 2,
        flag: 3 * 3,
        res: RES * 16,
        res_next: RES * 16,
        modu: MOD,
        bits2: 4 * 3,
    }
}

/// Buffers of a surface's coding: its words row by row, their choices and whether they were copied
/// whole, and an upsampling's.
pub struct Work {
    words: Vec<u64>,
    choices: Vec<u8>,
    whole: Vec<bool>,
    up: Up<u16>,
}

#[derive(Default)]
pub struct Planes {
    p: Option<Plane<u8, 4>>,
}

/// The fields of the channels A, R, G, B of a colour (A or B) of opacity `opaque`: their bits (0
/// of an opaque one's alpha) and their lowest bits in the word.
fn fields(b: bool, opaque: bool) -> [(u32, u32); 4] {
    match (b, opaque) {
        (false, true) => [(0, 0), (5, 42), (5, 37), (4, 33)],
        (false, false) => [(3, 44), (4, 40), (4, 36), (3, 33)],
        (true, true) => [(0, 0), (5, 58), (5, 53), (5, 48)],
        (true, false) => [(3, 60), (4, 56), (4, 52), (4, 48)],
    }
}

impl Pvrtc {
    /// The colours (bits 32 to 63) of the word at (bx, by) of a surface `bw` words wide, new
    /// (when encoding, `want`'s), predicted from the colours of the words around.
    fn colours(
        s: &mut impl Side,
        md: &mut Models,
        want: u64,
        words: &[u64],
        (bx, by, bw): (usize, usize, usize),
    ) -> u64 {
        let i = by * bw + bx;
        let left = (bx > 0).then(|| words[i - 1]);
        let up = (by > 0).then(|| words[i - bw]);
        let ul = (bx > 0 && by > 0).then(|| words[i - bw - 1]);
        let lm = left.map_or(2, |l| (l >> 32 & 1) as usize);
        let um = up.map_or(2, |u| (u >> 32 & 1) as usize);
        let mut w = (s.bit(&mut md.flag[lm * 3 + um], want >> 32 & 1 != 0, Cat::Mode) as u64) << 32;
        for (k, b) in [false, true].into_iter().enumerate() {
            let top = if b { 63 } else { 47 };
            let lo = left.map_or(2, |l| (l >> top & 1) as usize);
            let opaque =
                s.bit(&mut md.opaque[(k * 3 + lo) * 2 + (w >> 47 & 1) as usize * k], want >> top & 1 != 0, Cat::Mode);
            w |= (opaque as u64) << top;
            let nb = |n: Option<u64>| n.map(|n| colour(n, b));
            let (l, u, d) = (nb(left), nb(up), nb(ul));
            let mut rg = 0;
            // green, red, blue, then alpha (as ARGB 4555)
            let fs = fields(b, opaque);
            for (slot, c) in [2usize, 1, 3, 0].into_iter().enumerate() {
                let (bits, at) = fs[c];
                if bits == 0 {
                    continue;
                }
                let full = if c == 0 { 4 } else { 5 };
                let pred = match (l, u, d) {
                    (Some(l), Some(u), Some(d)) => med(l[c], u[c], d[c]),
                    (Some(l), _, _) => l[c],
                    (None, Some(u), _) => u[c],
                    _ => 1 << (full - 1),
                };
                let act = match (l, u) {
                    (Some(l), Some(u)) => ((l[c] - u[c]).unsigned_abs() as usize).min(3),
                    _ => 3,
                };
                let mut p = pred >> (full - bits);
                if slot == 1 || slot == 2 {
                    p += rg;
                }
                let before = if slot == 0 { 0 } else { class(rg) };
                let ctx = ((((k * 2 + opaque as usize) * 4 + slot) * 4 + act) * 4 + before) * 16;
                let v = zz(wrap(((want >> at) as i32 & ((1 << bits) - 1)) - p, bits));
                let v = s.token(&mut md.res[ctx..ctx + 16], &mut md.res_next[ctx..ctx + 16], v, Cat::Endpoints);
                let r = wrap(unzz(v), bits);
                w |= (((p + r) & ((1 << bits) - 1)) as u64) << at;
                if slot == 0 {
                    rg = r;
                }
            }
        }
        w
    }
}

/// The bucket of pixel `q` on the line from `a` to `b` (8-bit ARGB, the plane's RGBA swapped
/// back): of eighths of it, and one each side (0 to 10).
#[inline(always)]
fn bucket(ab: &[[i32; 4]; 2], q: [u8; 4], l2: i64) -> usize {
    let q = [q[3] as i32, q[0] as i32, q[1] as i32, q[2] as i32];
    let dot: i64 = (0..4).map(|c| (q[c] - ab[0][c]) as i64 * (ab[1][c] - ab[0][c]) as i64).sum();
    ((dot * 8 + l2 / 2).div_euclid(l2) + 1).clamp(0, 10) as usize
}

/// The modulation of word `words[(bx, by)]` (its colours known; when encoding, `want`'s), each
/// pixel's level by the images at it and the pixels predicted, set in `plane` as it decodes (of
/// 2-bit words of flag 1, the values stored only, the others as the word completes).
#[allow(clippy::too_many_arguments)]
fn modulation(
    s: &mut impl Side,
    md: &mut Models,
    want: u64,
    two: bool,
    (bx, by): (usize, usize),
    words: &[u64],
    size: (usize, usize),
    images: &[[[i32; 4]; 2]; 32],
    plane: &mut Plane<u8, 4>,
    coarse: Option<&Plane<u8, 4>>,
) -> u64 {
    let fw = if two { 8 } else { 4 };
    let mut w = words[by * size.0 + bx];
    let m = (w >> 32 & 1) as usize;
    // 2 bits a pixel of flag 1: its flags, the mode (bit 0) and of the horizontal and vertical
    // mode the way (bit 20)
    let hv = two && m == 1 && {
        let v = s.bit(&mut md.bits2[0], want & 1 != 0, Cat::Mode);
        w |= v as u64;
        if v {
            let f = s.bit(&mut md.bits2[1], want >> 20 & 1 != 0, Cat::Mode);
            w |= (f as u64) << 20;
        }
        v
    };
    for p in 0..fw * 4 {
        let (x, y) = (bx * fw + p % fw, by * 4 + p / fw);
        // where the pixel's value is, and of what kind
        let (lo, n, kind) = match (two, m) {
            (false, _) => (2 * p as u32, 2, m),
            (true, 0) => (p as u32, 1, 2),
            (true, _) => match stored(p % 8, p / 8, hv) {
                Some((lo, n)) => (lo, n, 3),
                None => continue,
            },
        };
        let ab = &images[p];
        let l2: i64 = (0..4).map(|c| ((ab[1][c] - ab[0][c]) as i64).pow(2)).sum();
        let (q1, q2) = match coarse {
            Some(c) => (Some(c.at(x, y)), plane.causal(x, y)),
            None => ((y > 0).then(|| plane.at(x, y - 1)), (x > 0).then(|| plane.at(x - 1, y))),
        };
        let bk = |q: Option<[u8; 4]>| match (q, l2) {
            (Some(q), 1..) => bucket(ab, q, l2),
            (Some(_), _) => 11,
            (None, _) => 11,
        };
        let contrast = match l2 {
            ..=300 => 0,
            301..=3000 => 1,
            _ => 2,
        };
        let ctx = (((kind * 3 + contrast) * 12 + bk(q1)) * 12 + bk(q2)) * 4;
        let v = (want >> lo) as u32 & ((1 << n) - 1);
        let v = match n {
            1 => s.tree_at(&mut md.modu[ctx..ctx + 4], 1, 1, v, Cat::Indices),
            _ => s.tree(&mut md.modu[ctx..ctx + 4], 2, v, Cat::Indices),
        };
        w |= (v as u64) << lo;
        let (wt, punch) = match (two, kind) {
            (false, _) => (WEIGHTS[m][v as usize], m == 1 && v == 2),
            (true, 2) => (8 * v as i32, false),
            _ => (WEIGHTS[0][if n == 1 { v * 3 } else { v } as usize], false),
        };
        plane.set(x, y, blend(ab, wt, punch));
    }
    w
}

impl Family for Pvrtc {
    const GROUP: Group = Group::Pvrtc;
    const STRIPED: bool = false;

    #[inline(always)]
    fn shape(f: Format) -> Shape {
        Shape { fw: f.footprint().0, fh: 4, least: 2, bytes: 8 }
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
        Work {
            words: Vec::with_capacity(blocks),
            choices: Vec::with_capacity(blocks),
            whole: Vec::with_capacity(blocks),
            up: Up::with_capacity(cols, across),
        }
    }

    fn work_plan(_: Format, blocks: usize, cols: usize, across: usize) -> u64 {
        use aqr_api::stack::block;
        block(8 * blocks as u64) + 2 * block(blocks as u64) + Up::<u16>::plan(cols, across)
    }

    fn across_values(_: Format) -> usize {
        4
    }

    fn planes_of(_: Format, _: &[u8], _: (usize, usize), _: &mut Planes) {
        unreachable!("no stripes of PVRTC")
    }

    fn upsampled(
        p: &Planes,
        _: Format,
        w: usize,
        rows: std::ops::Range<usize>,
        bufs: &mut Vec<[u8; 4]>,
        work: &mut Work,
    ) -> Planes {
        let take = std::mem::take(bufs);
        Planes { p: p.p.as_ref().map(|p| p.upsampled::<4>(w, rows, take, &mut work.up)) }
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
        let two = f == (Format::Pvrtc { two: true });
        let fw = if two { 8 } else { 4 };
        let mut plane = Plane::new_in(std::mem::take(bufs), bw * fw, bh * 4);
        let coarse = up.p.as_ref();
        let enc = s.encoding();
        let at = |x: usize, y: usize| morton(x, y, bw, bh) * 8;
        let word = |b: &[u8], x: usize, y: usize| u64::from_le_bytes(b[at(x, y)..at(x, y) + 8].try_into().unwrap());
        let (words, choices, whole) = (&mut work.words, &mut work.choices, &mut work.whole);
        words.clear();
        choices.clear();
        whole.clear();
        // the colours of every word, row by row
        let head = |b: &[u8; 8]| (u64::from_le_bytes(*b) >> 32) as u128;
        for by in 0..bh {
            for bx in 0..bw {
                let want = if enc { word(b, bx, by) } else { 0 };
                let i = by * bw + bx;
                let left = (bx > 0).then(|| words[i - 1].to_le_bytes());
                let upw = (by > 0).then(|| words[i - bw].to_le_bytes());
                let lc = if bx > 0 { choices[i - 1] } else { NONE };
                let uc = if by > 0 { choices[i - bw] } else { NONE };
                let (w, choice, all) =
                    match md.copies.code(s, &want.to_le_bytes(), left.as_ref(), upw.as_ref(), lc, uc, head)? {
                        Got::Block(c, choice) => (u64::from_le_bytes(c), choice, true),
                        Got::Head(c, choice) => (u64::from_le_bytes(c) & !0xffff_ffff, choice, false),
                        Got::New => (Pvrtc::colours(s, md, want, words, (bx, by, bw)), NEW, false),
                    };
                words.push(w);
                choices.push(choice);
                whole.push(all);
            }
        }
        // then the modulation of the words not copied whole, each pixel by the images there
        for by in 0..bh {
            for bx in 0..bw {
                let i = by * bw + bx;
                let ab = word_images(words, (bw, bh), fw, bx, by);
                if !whole[i] {
                    let want = if enc { word(b, bx, by) } else { 0 };
                    words[i] = modulation(s, md, want, two, (bx, by), words, (bw, bh), &ab, &mut plane, coarse);
                }
                let px = pixels(words, (bw, bh), two, bx, by, &ab);
                for p in 0..4 * fw {
                    plane.set(bx * fw + p % fw, by * 4 + p / fw, px[p]);
                }
                let w = words[i];
                md.copies.used(head(&w.to_le_bytes()), w.to_le_bytes());
                let k = at(bx, by);
                b[k..k + 8].copy_from_slice(&w.to_le_bytes());
            }
        }
        // (of 2 bits a pixel, the pixels between the values stored by the words around them)
        if two {
            exact2(words, (bw, bh), &mut plane);
        }
        Ok(Planes { p: Some(plane) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A word's images at once are those of each pixel, of both rates and of surfaces of one or
    /// two words a side too (where the words around wrap onto themselves).
    #[test]
    fn word_images_are_images() {
        let mut r = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = || {
            r ^= r << 13;
            r ^= r >> 7;
            r ^= r << 17;
            r
        };
        for (w, h) in [(1, 1), (2, 1), (1, 2), (2, 2), (4, 8), (8, 2)] {
            let words: Vec<u64> = (0..w * h).map(|_| next()).collect();
            for fw in [4, 8] {
                for by in 0..h {
                    for bx in 0..w {
                        let ab = word_images(&words, (w, h), fw, bx, by);
                        for p in 0..4 * fw {
                            let (x, y) = (bx * fw + p % fw, by * 4 + p / fw);
                            assert_eq!(ab[p], images(&words, (w, h), fw, x, y), "{w}x{h} {fw} ({x}, {y})");
                        }
                    }
                }
            }
        }
    }
}
