//! The models of the blocks' parts (FORMAT.md, "Models"), each over a surface at a time:
//! colour blocks ([`Colour`]: BC1, and BC2's and BC3's colours), BC4 blocks ([`Value`]: BC4,
//! BC3's alpha, BC5's red and green) and BC2's explicit alpha ([`Alpha`]).
//!
//! A block's endpoints are those of the block to its left, of the block above, of a block
//! before (by the rank of their last use, [`Recent`]) or new, predicted from those around. Its
//! indices are coded as levels on the line between its endpoints, by where the pixels around
//! predict them: the next smaller mipmap upsampled, which the surfaces before give (a layer's
//! mipmaps are coded from the smallest), and the pixels decoded before it on this surface
//! ([`Plane`]).

use aqr_common::arith::Bit;

use crate::bc;
use crate::coder::{Cat, Side};
use crate::{Result, corrupt};

/// A surface's pixels of `C` channels as its blocks decode, its sides whole blocks.
pub struct Plane<const C: usize> {
    w: usize,
    h: usize,
    px: Vec<[u8; C]>,
}

impl<const C: usize> Plane<C> {
    fn new(bw: usize, bh: usize) -> Self {
        Plane { w: bw * 4, h: bh * 4, px: vec![[0; C]; bw * bh * 16] }
    }

    #[inline(always)]
    fn at(&self, x: usize, y: usize) -> [u8; C] {
        self.px[y * self.w + x]
    }

    #[inline(always)]
    fn set(&mut self, x: usize, y: usize, v: [u8; C]) {
        self.px[y * self.w + x] = v;
    }

    /// The plane of the rows of blocks `rows` of a surface `bw` blocks wide from this one, of
    /// the next smaller mipmap, bilinearly: each pixel (x, y) of the surface is between four of
    /// this one, (x/2, y/2) and the one beside it towards (x, y) across and down (the sides
    /// repeated), weighted 9, 3, 3 and 1.
    pub fn upsampled(&self, bw: usize, rows: std::ops::Range<usize>) -> Plane<C> {
        let (w, y0, h) = (bw * 4, rows.start * 4, rows.len() * 4);
        let near = |v: usize, n: usize| {
            let c = (v / 2).min(n - 1);
            (c, if v.is_multiple_of(2) { c.saturating_sub(1) } else { (c + 1).min(n - 1) })
        };
        // across (3 · nearest + other), of the rows of this one that the rows taken are between,
        // then down (3 · nearest + other): 9, 3, 3, 1
        let lo = (y0 / 2).min(self.h - 1).saturating_sub(1);
        let hi = ((y0 + h).saturating_sub(1) / 2 + 1).min(self.h - 1);
        let mut across = vec![[0u16; C]; (hi + 1 - lo) * w];
        for (r, row) in across.chunks_exact_mut(w).enumerate() {
            let src = &self.px[(lo + r) * self.w..][..self.w];
            for (x, o) in row.iter_mut().enumerate() {
                let (c, n) = near(x, self.w);
                *o = std::array::from_fn(|k| 3 * src[c][k] as u16 + src[n][k] as u16);
            }
        }
        let mut px = vec![[0u8; C]; w * h];
        for (y, row) in px.chunks_exact_mut(w).enumerate() {
            let (c, n) = near(y0 + y, self.h);
            let (a, b) = (&across[(c - lo) * w..][..w], &across[(n - lo) * w..][..w]);
            for x in 0..w {
                row[x] = std::array::from_fn(|k| ((3 * a[x][k] + b[x][k] + 8) >> 4) as u8);
            }
        }
        Plane { w, h, px }
    }

    /// The planes of stripes of a surface, one above another, as the plane of the surface.
    pub fn stack(stripes: Vec<Plane<C>>) -> Plane<C> {
        let w = stripes.first().map_or(0, |p| p.w);
        let mut px = Vec::with_capacity(stripes.iter().map(|p| p.px.len()).sum());
        for p in stripes {
            debug_assert_eq!(p.w, w);
            px.extend(p.px);
        }
        Plane { w, h: px.len() / w.max(1), px }
    }

    /// Pixel (x, y) predicted by those before it: by MED (LOCO-I's median edge detector) of the
    /// left, upper and upper left ones, else the one there is of the left and upper ones.
    #[inline(always)]
    fn causal(&self, x: usize, y: usize) -> Option<[u8; C]> {
        let i = y * self.w + x;
        match (x > 0, y > 0) {
            (true, true) => {
                let (a, b, c) = (self.px[i - 1], self.px[i - self.w], self.px[i - self.w - 1]);
                Some(std::array::from_fn(|k| med(a[k] as i32, b[k] as i32, c[k] as i32) as u8))
            }
            (true, false) => Some(self.px[i - 1]),
            (false, true) => Some(self.px[i - self.w]),
            (false, false) => None,
        }
    }
}

/// MED: the median of `a`, `b` and `a + b − c` (without branches: pixels are not predictable).
#[inline(always)]
fn med(a: i32, b: i32, c: i32) -> i32 {
    a.min(b).max(a.max(b).min(a + b - c))
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

/// The bit length of `v`.
#[inline(always)]
fn len(v: u32) -> usize {
    (u32::BITS - v.leading_zeros()) as usize
}

/// Endpoints used before, by recency: the rank of a key is the number of other keys used since
/// it last was. Every block's endpoints are used once, in the order of the blocks.
///
/// Only ranks below [`RECENT`] are asked for, so only the most recent keys are kept: those of
/// the uses of a window of times, renumbered from 0 when it is full, the keys of rank
/// [`RECENT`] or more forgotten (which changes the rank of no other key).
#[derive(Clone)]
pub struct Recent {
    /// The time of each key's last use.
    last: std::collections::HashMap<u32, u32, Fast>,
    /// The key used at each time.
    keys: Vec<u32>,
    /// 1 at the times that are some key's last use, as a Fenwick tree.
    fen: Vec<u32>,
    live: u32,
}

/// Ranks of a hit of the cache are below this.
pub const RECENT: u32 = 4096;
/// Times of the window.
const WINDOW: usize = 4 * RECENT as usize;

/// A hash of a key by a multiplication: keys are not chosen against it (the encoder sees
/// them first, and a decoder of a crafted stream only runs slower).
#[derive(Default, Clone, Copy)]
pub struct Fast(u64);

impl std::hash::BuildHasher for Fast {
    type Hasher = Fast;
    fn build_hasher(&self) -> Fast {
        Fast(0)
    }
}

impl std::hash::Hasher for Fast {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, b: &[u8]) {
        for &x in b {
            self.write_u32(x as u32);
        }
    }
    fn write_u32(&mut self, v: u32) {
        self.0 = (self.0 ^ v as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15).rotate_left(29);
    }
}

impl Default for Recent {
    fn default() -> Self {
        Recent::new()
    }
}

impl Recent {
    pub fn new() -> Recent {
        Recent { last: Default::default(), keys: Vec::with_capacity(WINDOW), fen: vec![0; WINDOW + 1], live: 0 }
    }

    /// The window renumbered: the last [`RECENT`] keys used, in order, at times from 0.
    fn compact(&mut self) {
        let mut kept: Vec<u32> = Vec::with_capacity(RECENT as usize);
        for (t, &k) in self.keys.iter().enumerate().rev() {
            if kept.len() == RECENT as usize {
                break;
            }
            if self.last.get(&k) == Some(&(t as u32)) {
                kept.push(k);
            }
        }
        kept.reverse();
        self.last.clear();
        self.keys.clear();
        self.fen.fill(0);
        self.live = 0;
        for k in kept {
            self.touch(k);
        }
    }

    fn add(&mut self, t: usize, d: i32) {
        let mut i = t + 1;
        while i < self.fen.len() {
            self.fen[i] = self.fen[i].wrapping_add_signed(d);
            i += 1 << i.trailing_zeros();
        }
    }

    /// The keys last used at times below `t`.
    fn below(&self, t: usize) -> u32 {
        let (mut i, mut s) = (t, 0);
        while i > 0 {
            s += self.fen[i];
            i &= i - 1;
        }
        s
    }

    /// The rank of `key`, if it was used.
    pub fn rank(&self, key: u32) -> Option<u32> {
        let &t = self.last.get(&key)?;
        Some(self.live - self.below(t as usize + 1))
    }

    /// The key of rank `r`, if there is one.
    pub fn at(&self, r: u32) -> Option<u32> {
        // the (live − r)-th last use from the start, 1-based
        let mut k = self.live.checked_sub(r)?;
        if k == 0 {
            return None;
        }
        let (mut pos, mut step) = (0, (self.fen.len() - 1).next_power_of_two());
        while step > 0 {
            if pos + step < self.fen.len() && self.fen[pos + step] < k {
                pos += step;
                k -= self.fen[pos];
            }
            step /= 2;
        }
        Some(self.keys[pos])
    }

    /// Uses `key`, now.
    pub fn touch(&mut self, key: u32) {
        if self.keys.len() == WINDOW {
            self.compact();
        }
        let now = self.keys.len();
        match self.last.insert(key, now as u32) {
            Some(t) => self.add(t as usize, -1),
            None => self.live += 1,
        }
        self.keys.push(key);
        self.add(now, 1);
    }
}

/// Where a block's endpoints come from, as the contexts of the blocks after tell it: the left
/// block, the upper one, the cache, new, or none (no block there).
const LEFT: u8 = 0;
const UP: u8 = 1;
const CACHED: u8 = 2;
const NEW: u8 = 3;
const NONE: u8 = 4;

/// Bits of the choice of a block's endpoints (FORMAT.md, "Endpoints").
#[derive(Clone)]
struct Choice {
    left: Vec<Bit>,
    up: Vec<Bit>,
    hit: Vec<Bit>,
    rank: Vec<Bit>,
    rank_next: Vec<Bit>,
}

impl Choice {
    fn new() -> Choice {
        let v = |n| vec![Bit::default(); n];
        Choice { left: v(25 * 2), up: v(25), hit: v(5), rank: v(16), rank_next: v(16) }
    }

    /// The choice of a block whose endpoints are `key` (when encoding), between `left`, `up`
    /// and `recent`, the choices of the blocks there being `lc`, `uc`: the choice, and the
    /// endpoints where they are not new.
    #[inline(always)]
    #[allow(clippy::too_many_arguments)]
    fn code<S: Side>(
        &mut self,
        s: &mut S,
        key: u32,
        left: Option<u32>,
        up: Option<u32>,
        lc: u8,
        uc: u8,
        recent: &Recent,
    ) -> Result<(u8, Option<u32>)> {
        let ctx = lc as usize * 5 + uc as usize;
        let up = up.filter(|&u| Some(u) != left);
        if let Some(l) = left
            && s.bit(&mut self.left[ctx * 2 + up.is_some() as usize], key == l, Cat::Choice)
        {
            return Ok((LEFT, Some(l)));
        }
        if let Some(u) = up
            && s.bit(&mut self.up[ctx], key == u, Cat::Choice)
        {
            return Ok((UP, Some(u)));
        }
        let rank = if S::ENCODING { recent.rank(key).filter(|&r| r < RECENT) } else { None };
        if s.bit(&mut self.hit[lc as usize], rank.is_some(), Cat::Choice) {
            let r = s.token(&mut self.rank, &mut self.rank_next, rank.unwrap_or(0), Cat::Cache);
            let k = recent.at(r).filter(|_| r < RECENT).ok_or_else(corrupt)?;
            return Ok((CACHED, Some(k)));
        }
        Ok((NEW, None))
    }
}

/// The context bucket of a pixel's predicted level: `dot`, its prediction's offset from the
/// first endpoint along the line to the second, times `m` (FORMAT.md, "Indices").
#[inline(always)]
fn bucket(dot: i64, m: i64, sub: i64, max: usize) -> usize {
    (((dot * m + (1 << 16)) >> 17) + sub / 2).clamp(0, max as i64 - 1) as usize
}

/// The colour blocks of a texture: BC1's, or BC2's and BC3's (`four_always`: in four-colour
/// mode whatever the order of their endpoints).
#[derive(Clone)]
pub struct Colour {
    four_always: bool,
    choice: Choice,
    /// New endpoints' residuals: tokens and their next bits, by component and contexts.
    res: Vec<Bit>,
    res_next: Vec<Bit>,
    idx: Vec<Bit>,
    flat: Vec<Bit>,
}

/// Contexts of indices: the coarse and causal buckets (16 each), the contrast (4) and the mode.
const COLOUR_IDX: usize = 16 * 16 * 4 * 2;

impl Colour {
    /// The plane of the colour blocks at the start of each `stride` of `blocks`, of `bw` × `bh`
    /// blocks (as [`Colour::surface`] gives it).
    pub fn plane(four_always: bool, blocks: &[u8], stride: usize, (bw, bh): (usize, usize)) -> Plane<3> {
        let mut plane = Plane::<3>::new(bw, bh);
        for (i, b) in blocks.chunks(stride).take(bw * bh).enumerate() {
            let (c0, c1) = (u16::from_le_bytes([b[0], b[1]]), u16::from_le_bytes([b[2], b[3]]));
            let pal = bc::colours(c0, c1, four_always).map(|c| c.map(|v| v as u8));
            let ix = u32::from_le_bytes(b[4..8].try_into().unwrap());
            for p in 0..16 {
                plane.set(i % bw * 4 + p % 4, i / bw * 4 + p / 4, pal[(ix >> (2 * p) & 3) as usize]);
            }
        }
        plane
    }

    pub fn new(four_always: bool) -> Colour {
        let v = |n| vec![Bit::default(); n];
        Colour {
            four_always,
            choice: Choice::new(),
            res: v(6 * 8 * 8 * 16),
            res_next: v(6 * 8 * 8 * 16),
            idx: v(COLOUR_IDX * 4),
            flat: v(25 * 4),
        }
    }

    /// Codes the colour blocks of a surface of `bw` × `bh` blocks, the 8 bytes of each at the
    /// start of each `stride` of `blocks` (given when encoding, filled in when decoding), the
    /// surface before being `prev`; gives its plane.
    pub fn surface(
        &mut self,
        s: &mut impl Side,
        blocks: &mut [u8],
        stride: usize,
        (bw, bh): (usize, usize),
        prev: Option<&Plane<3>>,
        recent: &mut Recent,
    ) -> Result<Plane<3>> {
        let mut plane = Plane::<3>::new(bw, bh);
        let mut keys = vec![0u32; bw * bh];
        let mut choices = vec![NONE; bw * bh];
        for by in 0..bh {
            for bx in 0..bw {
                let i = by * bw + bx;
                let b: &mut [u8; 8] = (&mut blocks[i * stride..][..8]).try_into().unwrap();
                let key = u32::from_le_bytes(b[..4].try_into().unwrap());
                let left = (bx > 0).then(|| keys[i - 1]);
                let up = (by > 0).then(|| keys[i - bw]);
                let lc = if bx > 0 { choices[i - 1] } else { NONE };
                let uc = if by > 0 { choices[i - bw] } else { NONE };
                let (choice, known) = self.choice.code(s, key, left, up, lc, uc, recent)?;
                let key = match known {
                    Some(k) => k,
                    None => {
                        let ul = (bx > 0 && by > 0).then(|| keys[i - bw - 1]);
                        self.endpoints(s, key, left, up, ul)?
                    }
                };
                keys[i] = key;
                choices[i] = choice;
                recent.touch(key);
                b[..4].copy_from_slice(&key.to_le_bytes());
                let ix = self.indices(s, u32::from_le_bytes(b[4..].try_into().unwrap()), key, &mut plane, prev, bx, by);
                b[4..].copy_from_slice(&ix.to_le_bytes());
            }
        }
        Ok(plane)
    }

    /// New endpoints `key` (when encoding), by their components from those of the blocks
    /// around.
    #[inline(always)]
    fn endpoints(
        &mut self,
        s: &mut impl Side,
        key: u32,
        left: Option<u32>,
        up: Option<u32>,
        ul: Option<u32>,
    ) -> Result<u32> {
        const BITS: [u32; 3] = [5, 6, 5];
        let split = |k: u32| {
            let (c0, c1) = (k & 0xffff, k >> 16);
            [c0 >> 11, c0 >> 5 & 63, c0 & 31, c1 >> 11, c1 >> 5 & 63, c1 & 31].map(|v| v as i32)
        };
        let want = split(key);
        let (l, u, ul) = (left.map(split), up.map(split), ul.map(split));
        let mut cur = [0i32; 6];
        let mut gres = [0i32; 2];
        // green, red and blue of the first endpoint, then of the second relative to the first
        for (e, j) in [(0, 1), (0, 0), (0, 2), (1, 1), (1, 0), (1, 2)] {
            let c = e * 3 + j;
            let pick = |n: Option<[i32; 6]>| n.map(|n| if e == 1 { n[c] - n[j] } else { n[c] });
            let (pl, pu, pul) = (pick(l), pick(u), pick(ul));
            let pred = match (pl, pu, pul) {
                (Some(a), Some(b), Some(c)) => med(a, b, c),
                (Some(a), _, _) | (None, Some(a), _) => a,
                _ if e == 1 => 0,
                _ => 1 << (BITS[j] - 1),
            } + if e == 1 { cur[j] } else { 0 };
            let act = match (pl, pu) {
                (Some(a), Some(b)) => (a - b).unsigned_abs().min(6) as usize,
                _ => 7,
            };
            let gctx = if j == 1 { 0 } else { (gres[e] + 3).clamp(0, 6) as usize + 1 };
            let ctx = ((c * 8 + act) * 8 + gctx) * 16;
            let v = zz(wrap(want[c] - pred, BITS[j]));
            let v = s.token(&mut self.res[ctx..ctx + 16], &mut self.res_next[ctx..ctx + 16], v, Cat::Endpoints);
            if v >= 1 << BITS[j] {
                return Err(corrupt());
            }
            let r = unzz(v);
            cur[c] = (pred + r) & ((1 << BITS[j]) - 1);
            if j == 1 {
                gres[e] = r / 2;
            }
        }
        let c0 = (cur[0] << 11 | cur[1] << 5 | cur[2]) as u32;
        let c1 = (cur[3] << 11 | cur[4] << 5 | cur[5]) as u32;
        Ok(c0 | c1 << 16)
    }

    /// The indices `ix` (when encoding) of the block at (`bx`, `by`) of endpoints `key`, its
    /// pixels set in `plane`.
    #[inline(always)]
    #[allow(clippy::too_many_arguments)]
    fn indices(
        &mut self,
        s: &mut impl Side,
        ix: u32,
        key: u32,
        plane: &mut Plane<3>,
        prev: Option<&Plane<3>>,
        bx: usize,
        by: usize,
    ) -> u32 {
        let (c0, c1) = (key as u16, (key >> 16) as u16);
        let four = self.four_always || c0 > c1;
        let pal = bc::colours(c0, c1, self.four_always);
        let pal8 = pal.map(|c| c.map(|v| v as u8));
        let d = [0, 1, 2].map(|k| pal[1][k] - pal[0][k]);
        let l2 = d.iter().map(|v| v * v).sum::<i32>();
        let mut out = 0;
        if l2 == 0 {
            // (the endpoints' colours are one: the indices tell no pixel apart from another
            // but black)
            for p in 0..16 {
                let left = if p % 4 > 0 { (out >> (2 * p - 2) & 3) as usize + 1 } else { 0 };
                let up = if p >= 4 { (out >> (2 * p - 8) & 3) as usize + 1 } else { 0 };
                let ctx = (left * 5 + up) * 4;
                let i = s.tree(&mut self.flat[ctx..ctx + 4], 2, ix >> (2 * p) & 3, Cat::Indices);
                out |= i << (2 * p);
                plane.set(bx * 4 + p % 4, by * 4 + p / 4, pal8[i as usize]);
            }
            return out;
        }
        let contrast = match l2 {
            1..=100 => 1,
            101..=1000 => 2,
            _ => 3,
        };
        let (level, index, steps) =
            if four { (bc::COLOUR_LEVEL, bc::COLOUR_INDEX, 3) } else { (bc::COLOUR3_LEVEL, bc::COLOUR3_INDEX, 2) };
        let m = (steps << 19) / l2 as i64;
        let dot = |v: [u8; 3]| (0..3).map(|k| (v[k] as i32 - pal[0][k]) * d[k]).sum::<i32>() as i64;
        let base = (contrast * 2 + four as usize) * 256;
        for p in 0..16 {
            let (x, y) = (bx * 4 + p % 4, by * 4 + p / 4);
            let q1 = prev.map_or(15, |c| bucket(dot(c.at(x, y)), m, 4, 15));
            let q2 = plane.causal(x, y).map_or(15, |v| bucket(dot(v), m, 4, 15));
            let ctx = (base + q1 * 16 + q2) * 4;
            let lv = s.tree(&mut self.idx[ctx..ctx + 4], 2, level[(ix >> (2 * p) & 3) as usize] as u32, Cat::Indices);
            let i = index[lv as usize];
            out |= (i as u32) << (2 * p);
            plane.set(x, y, pal8[i as usize]);
        }
        out
    }
}

/// The BC4 blocks of a texture's channel.
#[derive(Clone)]
pub struct Value {
    choice: Choice,
    res: Vec<Bit>,
    res_next: Vec<Bit>,
    idx: Vec<Bit>,
    flat: Vec<Bit>,
}

/// Contexts of indices: the coarse and causal buckets (20 each), the contrast (4) and the mode.
const VALUE_IDX: usize = 20 * 20 * 4 * 2;

impl Value {
    /// As [`Colour::plane`], of BC4 blocks.
    pub fn plane(blocks: &[u8], stride: usize, (bw, bh): (usize, usize)) -> Plane<1> {
        let mut plane = Plane::<1>::new(bw, bh);
        for (i, b) in blocks.chunks(stride).take(bw * bh).enumerate() {
            let vals = bc::values(b[0], b[1]);
            let mut ix = [0; 8];
            ix[..6].copy_from_slice(&b[2..8]);
            let ix = u64::from_le_bytes(ix);
            for p in 0..16 {
                plane.set(i % bw * 4 + p % 4, i / bw * 4 + p / 4, [vals[(ix >> (3 * p) & 7) as usize] as u8]);
            }
        }
        plane
    }

    pub fn new() -> Value {
        let v = |n| vec![Bit::default(); n];
        Value {
            choice: Choice::new(),
            res: v(2 * 8 * 16 * 16),
            res_next: v(2 * 8 * 16 * 16),
            idx: v(VALUE_IDX * 8),
            flat: v(81 * 8),
        }
    }

    /// As [`Colour::surface`], of the BC4 blocks at the start of each `stride` of `blocks`.
    pub fn surface(
        &mut self,
        s: &mut impl Side,
        blocks: &mut [u8],
        stride: usize,
        (bw, bh): (usize, usize),
        prev: Option<&Plane<1>>,
        recent: &mut Recent,
    ) -> Result<Plane<1>> {
        let mut plane = Plane::<1>::new(bw, bh);
        let mut keys = vec![0u32; bw * bh];
        let mut choices = vec![NONE; bw * bh];
        for by in 0..bh {
            for bx in 0..bw {
                let i = by * bw + bx;
                let b: &mut [u8; 8] = (&mut blocks[i * stride..][..8]).try_into().unwrap();
                let key = u16::from_le_bytes([b[0], b[1]]) as u32;
                let left = (bx > 0).then(|| keys[i - 1]);
                let up = (by > 0).then(|| keys[i - bw]);
                let lc = if bx > 0 { choices[i - 1] } else { NONE };
                let uc = if by > 0 { choices[i - bw] } else { NONE };
                let (choice, known) = self.choice.code(s, key, left, up, lc, uc, recent)?;
                let key = match known {
                    Some(k) => k,
                    None => {
                        let ul = (bx > 0 && by > 0).then(|| keys[i - bw - 1]);
                        self.endpoints(s, key, left, up, ul)?
                    }
                };
                keys[i] = key;
                choices[i] = choice;
                recent.touch(key);
                b[..2].copy_from_slice(&(key as u16).to_le_bytes());
                let mut ix = [0; 8];
                ix[..6].copy_from_slice(&b[2..]);
                let ix = self.indices(s, u64::from_le_bytes(ix), key, &mut plane, prev, bx, by);
                b[2..].copy_from_slice(&ix.to_le_bytes()[..6]);
            }
        }
        Ok(plane)
    }

    /// New endpoints `key` (when encoding): the first from those of the blocks around, the
    /// second from the first by their differences.
    #[inline(always)]
    fn endpoints(
        &mut self,
        s: &mut impl Side,
        key: u32,
        left: Option<u32>,
        up: Option<u32>,
        ul: Option<u32>,
    ) -> Result<u32> {
        let split = |k: u32| [(k & 255) as i32, (k >> 8) as i32];
        let want = split(key);
        let (l, u, ul) = (left.map(split), up.map(split), ul.map(split));
        let mut cur = [0i32; 2];
        let mut r0 = 0;
        for e in 0..2 {
            let pick = |n: Option<[i32; 2]>| n.map(|n| if e == 1 { n[1] - n[0] } else { n[0] });
            let (pl, pu, pul) = (pick(l), pick(u), pick(ul));
            let pred = match (pl, pu, pul) {
                (Some(a), Some(b), Some(c)) => med(a, b, c),
                (Some(a), _, _) | (None, Some(a), _) => a,
                _ if e == 1 => 0,
                _ => 128,
            } + if e == 1 { cur[0] } else { 0 };
            let act = match (pl, pu) {
                (Some(a), Some(b)) => len((a - b).unsigned_abs()).min(6),
                _ => 7,
            };
            let rc = if e == 0 { 0 } else { len(zz(r0)).min(15) };
            let ctx = ((e * 8 + act) * 16 + rc) * 16;
            let v = zz(wrap(want[e] - pred, 8));
            let v = s.token(&mut self.res[ctx..ctx + 16], &mut self.res_next[ctx..ctx + 16], v, Cat::Endpoints);
            if v >= 256 {
                return Err(corrupt());
            }
            let r = unzz(v);
            cur[e] = (pred + r) & 255;
            if e == 0 {
                r0 = r;
            }
        }
        Ok((cur[0] | cur[1] << 8) as u32)
    }

    /// As [`Colour::indices`], of 3-bit indices.
    #[inline(always)]
    #[allow(clippy::too_many_arguments)]
    fn indices(
        &mut self,
        s: &mut impl Side,
        ix: u64,
        key: u32,
        plane: &mut Plane<1>,
        prev: Option<&Plane<1>>,
        bx: usize,
        by: usize,
    ) -> u64 {
        let (a0, a1) = (key as u8, (key >> 8) as u8);
        let vals = bc::values(a0, a1).map(|v| [v as u8]);
        let mut out = 0;
        let spread = a1 as i32 - a0 as i32;
        if spread == 0 {
            for p in 0..16 {
                let left = if p % 4 > 0 { (out >> (3 * p - 3) & 7) as usize + 1 } else { 0 };
                let up = if p >= 4 { (out >> (3 * p - 12) & 7) as usize + 1 } else { 0 };
                let ctx = (left * 9 + up) * 8;
                let i = s.tree(&mut self.flat[ctx..ctx + 8], 3, (ix >> (3 * p) & 7) as u32, Cat::Indices);
                out |= (i as u64) << (3 * p);
                plane.set(bx * 4 + p % 4, by * 4 + p / 4, vals[i as usize]);
            }
            return out;
        }
        let eight = a0 > a1;
        let contrast = match spread.unsigned_abs() {
            1..=8 => 1,
            9..=32 => 2,
            _ => 3,
        };
        let (level, index, steps) =
            if eight { (bc::VALUE_LEVEL, bc::VALUE_INDEX, 7) } else { (bc::VALUE6_LEVEL, bc::VALUE6_INDEX, 5) };
        let m = (steps << 18) / spread.unsigned_abs() as i64;
        let sign = spread.signum();
        let dot = |v: [u8; 1]| ((v[0] as i32 - a0 as i32) * sign) as i64;
        let base = (contrast * 2 + eight as usize) * 400;
        for p in 0..16 {
            let (x, y) = (bx * 4 + p % 4, by * 4 + p / 4);
            let q1 = prev.map_or(19, |c| bucket(dot(c.at(x, y)), m, 2, 19));
            let q2 = plane.causal(x, y).map_or(19, |v| bucket(dot(v), m, 2, 19));
            let ctx = (base + q1 * 20 + q2) * 8;
            let lv = s.tree(&mut self.idx[ctx..ctx + 8], 3, level[(ix >> (3 * p) & 7) as usize] as u32, Cat::Indices);
            let i = index[lv as usize];
            out |= (i as u64) << (3 * p);
            plane.set(x, y, vals[i as usize]);
        }
        out
    }
}

impl Default for Value {
    fn default() -> Self {
        Value::new()
    }
}

/// BC2's explicit alpha of a texture: 4-bit values.
#[derive(Clone)]
pub struct Alpha {
    a: Vec<Bit>,
}

impl Alpha {
    /// As [`Colour::plane`], of BC2's alpha.
    pub fn plane(blocks: &[u8], stride: usize, (bw, bh): (usize, usize)) -> Plane<1> {
        let mut plane = Plane::<1>::new(bw, bh);
        for (i, b) in blocks.chunks(stride).take(bw * bh).enumerate() {
            let v = u64::from_le_bytes(b[..8].try_into().unwrap());
            for p in 0..16 {
                plane.set(i % bw * 4 + p % 4, i / bw * 4 + p / 4, [(v >> (4 * p) & 15) as u8]);
            }
        }
        plane
    }

    pub fn new() -> Alpha {
        Alpha { a: vec![Bit::default(); 17 * 17 * 16] }
    }

    /// As [`Colour::surface`], of the 8 bytes of alpha at the start of each `stride` of
    /// `blocks`.
    pub fn surface(
        &mut self,
        s: &mut impl Side,
        blocks: &mut [u8],
        stride: usize,
        (bw, bh): (usize, usize),
        prev: Option<&Plane<1>>,
    ) -> Plane<1> {
        let mut plane = Plane::<1>::new(bw, bh);
        for by in 0..bh {
            for bx in 0..bw {
                let b: &mut [u8; 8] = (&mut blocks[(by * bw + bx) * stride..][..8]).try_into().unwrap();
                let v = u64::from_le_bytes(*b);
                let mut out = 0u64;
                for p in 0..16 {
                    let (x, y) = (bx * 4 + p % 4, by * 4 + p / 4);
                    let q1 = prev.map_or(16, |c| c.at(x, y)[0] as usize);
                    let q2 = plane.causal(x, y).map_or(16, |v| v[0] as usize);
                    let ctx = (q1 * 17 + q2) * 16;
                    let a = s.tree(&mut self.a[ctx..ctx + 16], 4, (v >> (4 * p) & 15) as u32, Cat::Alpha);
                    out |= (a as u64) << (4 * p);
                    plane.set(x, y, [a as u8]);
                }
                *b = out.to_le_bytes();
            }
        }
        plane
    }
}

impl Default for Alpha {
    fn default() -> Self {
        Alpha::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recent() {
        // a few keys, every rank checked; then many, across compactions of the window, the
        // ranks of the cache checked
        for (n, keys, every) in [(100, 13, 1), (5 * WINDOW, 6000, 997)] {
            let mut r = Recent::new();
            let mut mtf: Vec<u32> = vec![];
            let mut x = 7u32;
            for i in 0..n {
                x = x.wrapping_mul(1103515245).wrapping_add(12345);
                let key = (x >> 8) % keys;
                let want = mtf.iter().position(|&k| k == key).map(|p| p as u32).filter(|&p| p < RECENT);
                assert_eq!(r.rank(key).filter(|&p| p < RECENT), want);
                if i % every == 0 {
                    for (p, &k) in mtf.iter().take(RECENT as usize).enumerate() {
                        assert_eq!(r.at(p as u32), Some(k));
                    }
                    if mtf.len() < RECENT as usize {
                        assert_eq!(r.at(mtf.len() as u32), None);
                    }
                }
                if let Some(p) = mtf.iter().position(|&k| k == key) {
                    mtf.remove(p);
                }
                mtf.insert(0, key);
                r.touch(key);
            }
        }
    }
}
