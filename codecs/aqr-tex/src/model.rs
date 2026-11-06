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

/// Buffers that the models' surfaces and the planes upsampled work in, kept to be used again:
/// the endpoints and choices of a surface's blocks, and the columns and rows of an upsampling.
#[derive(Default)]
pub struct Work {
    keys: Vec<u32>,
    choices: Vec<u8>,
    cols: Vec<(u32, u32)>,
    across: Vec<u16>,
}

impl Work {
    /// Room for surfaces of `blocks` blocks, upsamplings to `cols` columns of `across` values.
    pub fn with_capacity(blocks: usize, cols: usize, across: usize) -> Work {
        Work {
            keys: Vec::with_capacity(blocks),
            choices: Vec::with_capacity(blocks),
            cols: Vec::with_capacity(cols),
            across: Vec::with_capacity(across),
        }
    }

    /// Bytes that [`Work::with_capacity`] takes of [`aqr_api::stack`].
    pub fn plan(blocks: usize, cols: usize, across: usize) -> u64 {
        use aqr_api::stack::block;
        block(4 * blocks as u64) + block(blocks as u64) + block(8 * cols as u64) + block(2 * across as u64)
    }

    /// The keys and choices of a surface of `n` blocks (none read before it is written).
    fn blocks(&mut self, n: usize) -> (&mut [u32], &mut [u8]) {
        self.keys.clear();
        self.keys.resize(n, 0);
        self.choices.clear();
        self.choices.resize(n, NONE);
        (&mut self.keys, &mut self.choices)
    }
}

/// The values of `across` that upsampling a plane of `h` rows to the rows from `y0`, `h1` of
/// them, takes for each value of a row (as [`Plane::upsampled`] does).
pub fn across_rows(h: usize, y0: usize, h1: usize) -> usize {
    let lo = (y0 / 2).min(h - 1).saturating_sub(1);
    let hi = ((y0 + h1).saturating_sub(1) / 2 + 1).min(h - 1);
    hi + 1 - lo
}

impl<const C: usize> Plane<C> {
    /// A plane of `bw` × `bh` blocks in `px` (cleared, and filled within its room).
    fn new_in(mut px: Vec<[u8; C]>, bw: usize, bh: usize) -> Self {
        px.clear();
        px.resize(bw * bh * 16, [0; C]);
        Plane { w: bw * 4, h: bh * 4, px }
    }

    /// The plane's buffer, to be used again.
    pub fn into_px(self) -> Vec<[u8; C]> {
        self.px
    }

    /// The plane of nothing yet of a surface `bw` blocks wide, in `px` (to be grown).
    pub fn empty_in(mut px: Vec<[u8; C]>, bw: usize) -> Self {
        px.clear();
        Plane { w: bw * 4, h: 0, px }
    }

    /// The plane grown by `bh` rows of blocks below it (within its room); the first of them.
    fn grow(&mut self, bw: usize, bh: usize) -> usize {
        debug_assert_eq!(self.w, bw * 4);
        let y0 = self.h;
        self.px.resize(self.px.len() + bw * bh * 16, [0; C]);
        self.h += bh * 4;
        y0
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
    /// repeated), weighted 9, 3, 3 and 1. In `px`, with `work`.
    pub fn upsampled(&self, bw: usize, rows: std::ops::Range<usize>, px: Vec<[u8; C]>, work: &mut Work) -> Plane<C> {
        let (w, y0, h) = (bw * 4, rows.start * 4, rows.len() * 4);
        let near = |v: usize, n: usize| {
            let c = (v / 2).min(n - 1);
            (c, if v.is_multiple_of(2) { c.saturating_sub(1) } else { (c + 1).min(n - 1) })
        };
        // across (3 · nearest + other), of the rows of this one that the rows taken are between,
        // then down (3 · nearest + other): 9, 3, 3, 1
        let lo = (y0 / 2).min(self.h - 1).saturating_sub(1);
        let hi = ((y0 + h).saturating_sub(1) / 2 + 1).min(self.h - 1);
        // (the columns of each x taken once for all rows; then rows of channels flat, which
        // the compiler vectorizes)
        let cols = &mut work.cols;
        cols.clear();
        cols.extend((0..w).map(|x| near(x, self.w)).map(|(c, n)| (c as u32, n as u32)));
        let wc = w * C;
        let across = &mut work.across;
        across.clear();
        across.resize((hi + 1 - lo) * wc, 0);
        for (r, row) in across.chunks_exact_mut(wc).enumerate() {
            let src = &self.px[(lo + r) * self.w..][..self.w];
            for (o, &(c, n)) in row.as_chunks_mut::<C>().0.iter_mut().zip(cols.iter()) {
                let (c, n) = (c as usize, n as usize);
                for k in 0..C {
                    o[k] = 3 * src[c][k] as u16 + src[n][k] as u16;
                }
            }
        }
        let mut px = px;
        px.clear();
        px.resize(w * h, [0u8; C]);
        for (y, row) in px.chunks_exact_mut(w).enumerate() {
            let (c, n) = near(y0 + y, self.h);
            let (a, b) = (&across[(c - lo) * wc..][..wc], &across[(n - lo) * wc..][..wc]);
            for ((o, &a), &b) in row.as_flattened_mut().iter_mut().zip(a).zip(b) {
                *o = ((3 * a + b + 8) >> 4) as u8;
            }
        }
        Plane { w, h, px }
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
    /// The time of each key's last use: a table of [`SLOTS`] (key, time), by open addressing
    /// (a time of [`EMPTY`] for none), at most half of them used.
    last: Vec<(u32, u32)>,
    /// The key used at each time.
    keys: Vec<u32>,
    /// 1 at the times that are some key's last use, a bit each (`WINDOW / 64` words), and the
    /// count of them in each group of [`GROUP`] words: a use changes two bits and two counts,
    /// whatever the window, and a rank is counted over at most a group's words and the groups.
    bits: Vec<u64>,
    groups: Vec<u32>,
    live: u32,
}

/// Ranks of a hit of the cache are below this.
pub const RECENT: u32 = 4096;
/// Times of the window.
const WINDOW: usize = 4 * RECENT as usize;
/// Words of a group of times.
const GROUP: usize = 16;
const GROUPS: usize = WINDOW / 64 / GROUP;
/// Entries of the table of keys (twice the keys of a window), and the time of an empty one.
const SLOTS: usize = 2 * WINDOW;
const EMPTY: u32 = u32::MAX;

/// The slot of a key by a multiplication: keys are not chosen against it (the encoder sees
/// them first, and a decoder of a crafted stream only runs slower).
#[inline(always)]
fn slot(key: u32) -> usize {
    ((key as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15) >> 40) as usize & (SLOTS - 1)
}

impl Default for Recent {
    fn default() -> Self {
        Recent::new()
    }
}

impl Recent {
    pub fn new() -> Recent {
        Recent {
            last: vec![(0, EMPTY); SLOTS],
            keys: Vec::with_capacity(WINDOW),
            bits: vec![0; WINDOW / 64],
            groups: vec![0; GROUPS],
            live: 0,
        }
    }

    /// As [`Colour::copy_from`].
    pub fn copy_from(&mut self, o: &Recent) {
        self.last.clone_from(&o.last);
        self.keys.clone_from(&o.keys);
        self.bits.clone_from(&o.bits);
        self.groups.clone_from(&o.groups);
        self.live = o.live;
    }

    /// Bytes that [`Recent::new`] takes of [`aqr_api::stack`].
    pub fn plan() -> u64 {
        use aqr_api::stack::block;
        block(8 * SLOTS as u64) + block(4 * WINDOW as u64) + block(WINDOW as u64 / 8) + block(4 * GROUPS as u64)
    }

    /// The table's entry of `key`: where it is, or where it goes.
    #[inline(always)]
    fn find(&self, key: u32) -> usize {
        let mut i = slot(key);
        while self.last[i].1 != EMPTY && self.last[i].0 != key {
            i = (i + 1) & (SLOTS - 1);
        }
        i
    }

    /// The time of `key`'s last use, if any.
    #[inline(always)]
    fn get(&self, key: u32) -> Option<u32> {
        let (_, t) = self.last[self.find(key)];
        (t != EMPTY).then_some(t)
    }

    /// The window renumbered: the last [`RECENT`] keys used, in order, at times from 0. (They
    /// are gathered at the end of the window as it is read from the end, then moved to its
    /// start.)
    fn compact(&mut self) {
        let mut n = 0;
        for t in (0..self.keys.len()).rev() {
            if n == RECENT as usize {
                break;
            }
            let k = self.keys[t];
            if self.get(k) == Some(t as u32) {
                n += 1;
                let at = self.keys.len() - n;
                self.keys[at] = k;
            }
        }
        let from = self.keys.len() - n;
        self.keys.copy_within(from.., 0);
        self.keys.truncate(n);
        self.last.fill((0, EMPTY));
        self.bits.fill(0);
        self.groups.fill(0);
        self.live = 0;
        for t in 0..n {
            let k = self.keys[t];
            let i = self.find(k);
            self.last[i] = (k, t as u32);
            self.live += 1;
            self.flip(t, 1);
        }
    }

    /// Flips time `t`, set (`d` = 1) or cleared (−1).
    #[inline]
    fn flip(&mut self, t: usize, d: i32) {
        self.bits[t / 64] ^= 1 << (t % 64);
        let g = &mut self.groups[t / 64 / GROUP];
        *g = g.wrapping_add_signed(d);
    }

    /// The keys last used at times after `t`.
    fn after(&self, t: usize) -> u32 {
        let (w, g) = (t / 64, t / 64 / GROUP);
        // (over whole groups and words, masked, rather than loops of bounds as unpredictable)
        let words: u32 = self.bits[g * GROUP..][..GROUP]
            .iter()
            .enumerate()
            .map(|(i, &b)| if i > w % GROUP { b.count_ones() } else { 0 })
            .sum();
        let groups: u32 = self.groups.iter().enumerate().map(|(i, &c)| if i > g { c } else { 0 }).sum();
        (self.bits[w] >> (t % 64) >> 1).count_ones() + words + groups
    }

    /// The rank of `key`, if it was used.
    pub fn rank(&self, key: u32) -> Option<u32> {
        let t = self.get(key)?;
        Some(self.after(t as usize))
    }

    /// The key of rank `r`, if there is one.
    pub fn at(&self, r: u32) -> Option<u32> {
        if r >= self.live {
            return None;
        }
        // the (r + 1)-th last use from the end: its group, its word, then its bit
        let mut k = r;
        let mut g = GROUPS - 1;
        while self.groups[g] <= k {
            k -= self.groups[g];
            g -= 1;
        }
        let mut w = g * GROUP + GROUP - 1;
        while self.bits[w].count_ones() <= k {
            k -= self.bits[w].count_ones();
            w -= 1;
        }
        // the highest bit p with more than k set from it up
        let (b, mut p) = (self.bits[w], 0);
        for s in [32, 16, 8, 4, 2, 1] {
            if (b >> (p + s)).count_ones() > k {
                p += s;
            }
        }
        Some(self.keys[w * 64 + p as usize])
    }

    /// Uses `key`, now.
    pub fn touch(&mut self, key: u32) {
        if self.keys.len() == WINDOW {
            self.compact();
        }
        let now = self.keys.len();
        let i = self.find(key);
        match std::mem::replace(&mut self.last[i], (key, now as u32)).1 {
            EMPTY => self.live += 1,
            t => self.flip(t as usize, -1),
        }
        self.keys.push(key);
        self.flip(now, 1);
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

/// Bytes of [`aqr_api::stack`] of a vector of `n` adaptive bits.
fn bits(n: usize) -> u64 {
    aqr_api::stack::block((n * size_of::<Bit>()) as u64)
}

impl Choice {
    const SIZES: [usize; 5] = [25 * 2, 25, 5, 16, 16];

    fn new() -> Choice {
        let v = |n| vec![Bit::default(); n];
        let [l, u, h, r, n] = Self::SIZES;
        Choice { left: v(l), up: v(u), hit: v(h), rank: v(r), rank_next: v(n) }
    }

    fn plan() -> u64 {
        Self::SIZES.iter().map(|&n| bits(n)).sum()
    }

    fn copy_from(&mut self, o: &Choice) {
        self.left.clone_from(&o.left);
        self.up.clone_from(&o.up);
        self.hit.clone_from(&o.hit);
        self.rank.clone_from(&o.rank);
        self.rank_next.clone_from(&o.rank_next);
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

/// The context buckets of pixels' predicted levels (FORMAT.md, "Indices"): of a pixel `v`, the
/// offset of its prediction from the first endpoint `e` along the line `d` to the second, times
/// `m`, in 1/2^17, rounded, plus `sub / 2`, within `0..max`. The products by `m` are taken
/// beforehand, one multiplication per channel the less on the decoder's chain from pixel to
/// pixel (the sums are the same).
#[derive(Clone, Copy)]
struct Buckets<const C: usize> {
    dm: [i64; C],
    off: i64,
    max: i64,
}

impl<const C: usize> Buckets<C> {
    #[inline(always)]
    fn new(e: [i32; C], d: [i32; C], m: i64, max: usize) -> Self {
        let dm = d.map(|d| d as i64 * m);
        let off = (1 << 16) - (0..C).map(|k| e[k] as i64 * dm[k]).sum::<i64>();
        Buckets { dm, off, max: max as i64 - 1 }
    }

    #[inline(always)]
    fn of(&self, v: [u8; C], sub: i64) -> usize {
        let proj = (0..C).map(|k| v[k] as i64 * self.dm[k]).sum::<i64>() + self.off;
        ((proj >> 17) + sub / 2).clamp(0, self.max) as usize
    }
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
    pub fn plane(four_always: bool, blocks: &[u8], stride: usize, (bw, bh): (usize, usize), plane: &mut Plane<3>) {
        let y0 = plane.grow(bw, bh);
        for (i, b) in blocks.chunks(stride).take(bw * bh).enumerate() {
            let (c0, c1) = (u16::from_le_bytes([b[0], b[1]]), u16::from_le_bytes([b[2], b[3]]));
            let pal = bc::colours(c0, c1, four_always).map(|c| c.map(|v| v as u8));
            let ix = u32::from_le_bytes(b[4..8].try_into().unwrap());
            for p in 0..16 {
                plane.set(i % bw * 4 + p % 4, y0 + i / bw * 4 + p / 4, pal[(ix >> (2 * p) & 3) as usize]);
            }
        }
    }

    const SIZES: [usize; 4] = [6 * 8 * 8 * 16, 6 * 8 * 8 * 16, COLOUR_IDX * 4, 25 * 4];

    pub fn new(four_always: bool) -> Colour {
        let v = |n| vec![Bit::default(); n];
        let [r, n, i, f] = Self::SIZES;
        Colour { four_always, choice: Choice::new(), res: v(r), res_next: v(n), idx: v(i), flat: v(f) }
    }

    /// Bytes that [`Colour::new`] takes of [`aqr_api::stack`].
    pub fn plan() -> u64 {
        Choice::plan() + Self::SIZES.iter().map(|&n| bits(n)).sum::<u64>()
    }

    /// These models made those of `o`, in their vectors.
    pub fn copy_from(&mut self, o: &Colour) {
        self.four_always = o.four_always;
        self.choice.copy_from(&o.choice);
        self.res.clone_from(&o.res);
        self.res_next.clone_from(&o.res_next);
        self.idx.clone_from(&o.idx);
        self.flat.clone_from(&o.flat);
    }

    /// Codes the colour blocks of a surface of `bw` × `bh` blocks, the 8 bytes of each at the
    /// start of each `stride` of `blocks` (given when encoding, filled in when decoding), the
    /// surface before being `prev`; gives its plane (in `px`, with `work`).
    #[allow(clippy::too_many_arguments)]
    pub fn surface(
        &mut self,
        s: &mut impl Side,
        blocks: &mut [u8],
        stride: usize,
        (bw, bh): (usize, usize),
        prev: Option<&Plane<3>>,
        recent: &mut Recent,
        (px, work): (Vec<[u8; 3]>, &mut Work),
    ) -> Result<Plane<3>> {
        let mut plane = Plane::<3>::new_in(px, bw, bh);
        let (keys, choices) = work.blocks(bw * bh);
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
        // (the pixel of each level, a lookup the less after each index)
        let by_level: [[u8; 3]; 4] = std::array::from_fn(|lv| pal8[index[lv] as usize]);
        let m = (steps << 19) / l2 as i64;
        let q = Buckets::new(pal[0], d, m, 15);
        let base = (contrast * 2 + four as usize) * 256;
        for p in 0..16 {
            let (x, y) = (bx * 4 + p % 4, by * 4 + p / 4);
            let q1 = prev.map_or(15, |c| q.of(c.at(x, y), 4));
            let q2 = plane.causal(x, y).map_or(15, |v| q.of(v, 4));
            let ctx = (base + q1 * 16 + q2) * 4;
            let lv = s.tree(&mut self.idx[ctx..ctx + 4], 2, level[(ix >> (2 * p) & 3) as usize] as u32, Cat::Indices);
            out |= (index[lv as usize] as u32) << (2 * p);
            plane.set(x, y, by_level[lv as usize]);
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
    pub fn plane(blocks: &[u8], stride: usize, (bw, bh): (usize, usize), plane: &mut Plane<1>) {
        let y0 = plane.grow(bw, bh);
        for (i, b) in blocks.chunks(stride).take(bw * bh).enumerate() {
            let vals = bc::values(b[0], b[1]);
            let mut ix = [0; 8];
            ix[..6].copy_from_slice(&b[2..8]);
            let ix = u64::from_le_bytes(ix);
            for p in 0..16 {
                plane.set(i % bw * 4 + p % 4, y0 + i / bw * 4 + p / 4, [vals[(ix >> (3 * p) & 7) as usize] as u8]);
            }
        }
    }

    const SIZES: [usize; 4] = [2 * 8 * 16 * 16, 2 * 8 * 16 * 16, VALUE_IDX * 8, 81 * 8];

    pub fn new() -> Value {
        let v = |n| vec![Bit::default(); n];
        let [r, n, i, f] = Self::SIZES;
        Value { choice: Choice::new(), res: v(r), res_next: v(n), idx: v(i), flat: v(f) }
    }

    /// Bytes that [`Value::new`] takes of [`aqr_api::stack`].
    pub fn plan() -> u64 {
        Choice::plan() + Self::SIZES.iter().map(|&n| bits(n)).sum::<u64>()
    }

    /// As [`Colour::copy_from`].
    pub fn copy_from(&mut self, o: &Value) {
        self.choice.copy_from(&o.choice);
        self.res.clone_from(&o.res);
        self.res_next.clone_from(&o.res_next);
        self.idx.clone_from(&o.idx);
        self.flat.clone_from(&o.flat);
    }

    /// As [`Colour::surface`], of the BC4 blocks at the start of each `stride` of `blocks`.
    #[allow(clippy::too_many_arguments)]
    pub fn surface(
        &mut self,
        s: &mut impl Side,
        blocks: &mut [u8],
        stride: usize,
        (bw, bh): (usize, usize),
        prev: Option<&Plane<1>>,
        recent: &mut Recent,
        (px, work): (Vec<[u8; 1]>, &mut Work),
    ) -> Result<Plane<1>> {
        let mut plane = Plane::<1>::new_in(px, bw, bh);
        let (keys, choices) = work.blocks(bw * bh);
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
        let by_level: [[u8; 1]; 8] = std::array::from_fn(|lv| vals[index[lv] as usize]);
        let m = (steps << 18) / spread.unsigned_abs() as i64;
        let q = Buckets::new([a0 as i32], [spread.signum()], m, 19);
        let base = (contrast * 2 + eight as usize) * 400;
        for p in 0..16 {
            let (x, y) = (bx * 4 + p % 4, by * 4 + p / 4);
            let q1 = prev.map_or(19, |c| q.of(c.at(x, y), 2));
            let q2 = plane.causal(x, y).map_or(19, |v| q.of(v, 2));
            let ctx = (base + q1 * 20 + q2) * 8;
            let lv = s.tree(&mut self.idx[ctx..ctx + 8], 3, level[(ix >> (3 * p) & 7) as usize] as u32, Cat::Indices);
            out |= (index[lv as usize] as u64) << (3 * p);
            plane.set(x, y, by_level[lv as usize]);
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
    pub fn plane(blocks: &[u8], stride: usize, (bw, bh): (usize, usize), plane: &mut Plane<1>) {
        let y0 = plane.grow(bw, bh);
        for (i, b) in blocks.chunks(stride).take(bw * bh).enumerate() {
            let v = u64::from_le_bytes(b[..8].try_into().unwrap());
            for p in 0..16 {
                plane.set(i % bw * 4 + p % 4, y0 + i / bw * 4 + p / 4, [(v >> (4 * p) & 15) as u8]);
            }
        }
    }

    pub fn new() -> Alpha {
        Alpha { a: vec![Bit::default(); 17 * 17 * 16] }
    }

    /// Bytes that [`Alpha::new`] takes of [`aqr_api::stack`].
    pub fn plan() -> u64 {
        bits(17 * 17 * 16)
    }

    /// As [`Colour::copy_from`].
    pub fn copy_from(&mut self, o: &Alpha) {
        self.a.clone_from(&o.a);
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
        px: Vec<[u8; 1]>,
    ) -> Plane<1> {
        let mut plane = Plane::<1>::new_in(px, bw, bh);
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
