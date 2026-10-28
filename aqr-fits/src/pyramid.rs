//! The pyramid mode of the pixel model (FORMAT.md, "The pyramid"): an image taken apart level
//! by level into the sums and differences of its 2×2 blocks (an H-transform, exactly
//! reversible), each level's blocks at the phase that codes them the best. Images that went
//! through such a transform with quantised coefficients (hcompress's, as the Digitized Sky
//! Survey's) code far smaller so than by prediction.
//!
//! A level of `w` × `h` values at phase `(px, py)` has blocks of rows `py + 2i`, `py + 2i + 1`
//! and columns `px + 2j`, `px + 2j + 1`; the values outside them are its border. Of a block of
//! `a00 a01` over `a10 a11`, the coefficients are
//!
//! ```text
//! hx = a11 + a10 − a01 − a00,  hy = a11 − a10 + a01 − a00,  hc = a11 − a10 − a01 + a00
//! h0 = a00 + a01 + a10 + a11
//! ```
//!
//! of which `hc`, `(hx − p) / 2` and `(hy − p) / 2` (`p` the parity of `hc`, which all four
//! share) are coded, and `h0 >> 2` is the next level's value: `h0 mod 4` follows from the
//! others. The levels end where a side is shorter than 4, and their last values are coded
//! by their differences.

use crate::bits;
use crate::image::Coded;
use aqr_common::arith::{ArithDecoder, ArithEncoder, Bit};

/// Levels at most.
const LEVELS: usize = 24;
/// Activity buckets of the coefficients' contexts.
const BUCKETS: usize = 24;
/// Contexts: of the three bands, three level classes (0, 1, 2 and more), and the buckets; then
/// of the borders and of the last level, the buckets each.
const COEF: usize = 3 * 3 * BUCKETS;
const BORDER: usize = COEF;
const TOP: usize = COEF + BUCKETS;
const CONTEXTS: usize = TOP + BUCKETS;
/// Lengths of a number in bits, 0 to 63, as a tree of 6 bits.
const LEN_TREE: usize = 64;
/// Bits after the leading one that are coded with adaptive bits, at most: each in the context
/// of the bits before it (and the length), so that any distribution of values of a band and
/// level is learnt (the lattices of quantised coefficients).
const MODELLED: u32 = 10;
/// Lengths told apart by the contexts of those bits (longer ones share the last).
const MANT_LENS: usize = 24;
/// Adaptive bits of those of a context: a tree of `2^min(n − 1, MODELLED)` per length `n`.
const MANT_NODES: usize = MANT_LENS << MODELLED;
/// Contexts of those bits: of the three bands at eight level classes, the borders and the
/// last level.
const MANT_CONTEXTS: usize = 3 * 8 + 2;

/// The half octave of `t`: 0, 1, then two buckets per octave.
#[inline(always)]
fn bucket(t: u64) -> usize {
    let l = 64 - t.leading_zeros();
    let b = if l < 2 { l as usize } else { (2 * l - 1 + ((t >> (l - 2)) & 1) as u32) as usize };
    b.min(BUCKETS - 1)
}

#[inline(always)]
fn zigzag(v: i64) -> u64 {
    ((v << 1) ^ (v >> 63)) as u64
}

#[inline(always)]
fn unzigzag(z: u64) -> i64 {
    ((z >> 1) as i64) ^ -((z & 1) as i64)
}

/// The adaptive bit of the bits after the leading one of a length `n`, at `node` of their
/// tree, in context `mctx`.
#[inline(always)]
fn mant_at(mctx: usize, n: u32, node: usize) -> usize {
    (mctx * MANT_LENS + (n as usize).min(MANT_LENS - 1)) << MODELLED | node
}

/// The context of the bits after the leading one of a coefficient of `band` at level `lv`.
#[inline(always)]
fn mant_ctx(band: usize, lv: usize) -> usize {
    band * 8 + lv.min(7)
}

const MANT_BORDER: usize = 24;
const MANT_TOP: usize = 25;

/// The adaptive models, alike on both sides.
struct Models {
    len: Vec<Bit>,
    mant: Vec<Bit>,
    phase: [Bit; 2],
}

impl Models {
    fn new() -> Models {
        Models {
            len: vec![Bit::default(); CONTEXTS * LEN_TREE],
            mant: vec![Bit::default(); MANT_CONTEXTS * MANT_NODES],
            phase: [Bit::default(); 2],
        }
    }
}

struct Encoder {
    m: Models,
    ac: ArithEncoder,
    raw: bits::Writer,
}

impl Encoder {
    fn bit(&mut self, b: bool, p: &mut Bit) {
        self.ac.encode(b, p.p1());
        p.update(b);
    }

    /// Codes `v` in context `ctx`: its zigzag's length, its bits after the leading one (the
    /// first [`MODELLED`] adaptive, in context `mctx`, by the length and the bits before), the
    /// rest raw.
    fn put(&mut self, ctx: usize, mctx: usize, v: i64) {
        let z = zigzag(v);
        let n = 64 - z.leading_zeros();
        let tree = &mut self.m.len[ctx * LEN_TREE..][..LEN_TREE];
        let mut node = 1;
        for i in (0..6).rev() {
            let b = (n >> i) & 1 != 0;
            self.ac.encode(b, tree[node].p1());
            tree[node].update(b);
            node = node * 2 + b as usize;
        }
        if n < 2 {
            return;
        }
        let mut node = 1;
        let modelled = (n - 1).min(MODELLED);
        for i in 0..modelled {
            let b = (z >> (n - 2 - i)) & 1 != 0;
            let p = &mut self.m.mant[mant_at(mctx, n, node)];
            self.ac.encode(b, p.p1());
            p.update(b);
            node = node * 2 + b as usize;
        }
        let rest = n - 1 - modelled;
        if rest > 32 {
            self.raw.put((z >> 32) as u32, rest - 32);
            self.raw.put(z as u32, 32);
        } else {
            self.raw.put(z as u32, rest);
        }
    }
}

struct Decoder<'a> {
    m: Models,
    ac: ArithDecoder<'a>,
    raw: bits::Reader<'a>,
}

impl Decoder<'_> {
    fn bit(&mut self, p: &mut Bit) -> bool {
        let b = self.ac.decode(p.p1());
        p.update(b);
        b
    }

    fn get(&mut self, ctx: usize, mctx: usize) -> i64 {
        let tree = &mut self.m.len[ctx * LEN_TREE..][..LEN_TREE];
        let mut node = 1;
        for _ in 0..6 {
            let b = self.ac.decode(tree[node].p1());
            tree[node].update(b);
            node = node * 2 + b as usize;
        }
        let n = (node - LEN_TREE) as u32;
        if n < 2 {
            return unzigzag(n as u64);
        }
        let mut node = 1;
        let modelled = (n - 1).min(MODELLED);
        for _ in 0..modelled {
            let p = &mut self.m.mant[mant_at(mctx, n, node)];
            let b = self.ac.decode(p.p1());
            p.update(b);
            node = node * 2 + b as usize;
        }
        let rest = n - 1 - modelled;
        let low = if rest > 32 {
            (self.raw.get(rest - 32) as u64) << 32 | self.raw.get(32) as u64
        } else {
            self.raw.get(rest) as u64
        };
        // (the leading one and the modelled bits above the rest)
        unzigzag((node as u64) << rest | low)
    }
}

/// A level: its size and phase, and the size of the next one.
#[derive(Clone, Copy, Debug)]
struct Level {
    w: usize,
    h: usize,
    px: usize,
    py: usize,
}

impl Level {
    /// The next level's size (the blocks).
    fn blocks(&self) -> (usize, usize) {
        ((self.w - self.px) / 2, (self.h - self.py) / 2)
    }

    fn in_blocks(&self, x: usize, y: usize) -> bool {
        let (bw, bh) = self.blocks();
        x >= self.px && x < self.px + 2 * bw && y >= self.py && y < self.py + 2 * bh
    }
}

/// Whether a level of `w` × `h` is taken apart.
fn splits(w: usize, h: usize, levels: usize) -> bool {
    w >= 4 && h >= 4 && levels < LEVELS
}

/// The coefficients of a level's blocks, as coded: `hc`, `hx'`, `hy'` per block, and the next
/// level's values.
struct Bands {
    c: [Vec<i64>; 3],
    next: Vec<i64>,
}

fn transform(v: &[i64], l: &Level) -> Bands {
    let (bw, bh) = l.blocks();
    let mut b = Bands { c: [vec![0; bw * bh], vec![0; bw * bh], vec![0; bw * bh]], next: vec![0; bw * bh] };
    for i in 0..bh {
        let r0 = (l.py + 2 * i) * l.w + l.px;
        let r1 = r0 + l.w;
        for j in 0..bw {
            let (a00, a01, a10, a11) = (v[r0 + 2 * j], v[r0 + 2 * j + 1], v[r1 + 2 * j], v[r1 + 2 * j + 1]);
            let hx = a11 + a10 - a01 - a00;
            let hy = a11 - a10 + a01 - a00;
            let hc = a11 - a10 - a01 + a00;
            let h0 = a00 + a01 + a10 + a11;
            let p = hc & 1;
            let k = i * bw + j;
            b.c[0][k] = hc;
            b.c[1][k] = (hx - p) >> 1;
            b.c[2][k] = (hy - p) >> 1;
            b.next[k] = h0 >> 2;
        }
    }
    b
}

/// The values of the level's blocks from its bands.
fn inverse(b: &Bands, l: &Level, v: &mut [i64]) {
    let (bw, bh) = l.blocks();
    for i in 0..bh {
        let r0 = (l.py + 2 * i) * l.w + l.px;
        let r1 = r0 + l.w;
        for j in 0..bw {
            let k = i * bw + j;
            let hc = b.c[0][k];
            let p = hc & 1;
            let hx = b.c[1][k].wrapping_mul(2).wrapping_add(p);
            let hy = b.c[2][k].wrapping_mul(2).wrapping_add(p);
            let r = (hx.wrapping_add(hy).wrapping_sub(hc)).rem_euclid(4);
            let h0 = b.next[k].wrapping_mul(4).wrapping_add(r);
            v[r0 + 2 * j] = h0.wrapping_sub(hx).wrapping_sub(hy).wrapping_add(hc) >> 2;
            v[r0 + 2 * j + 1] = h0.wrapping_sub(hx).wrapping_add(hy).wrapping_sub(hc) >> 2;
            v[r1 + 2 * j] = h0.wrapping_add(hx).wrapping_sub(hy).wrapping_sub(hc) >> 2;
            v[r1 + 2 * j + 1] = h0.wrapping_add(hx).wrapping_add(hy).wrapping_add(hc) >> 2;
        }
    }
}

/// The context of coefficient `k` of band `band` at level `lv` (whose blocks are `bw` wide):
/// the magnitudes of its neighbours left and above in the band, of the coefficients of its
/// block coded before it, and of its parent (the band's coefficient of the block above it, at
/// the next level, if any).
fn coef_ctx(b: &Bands, band: usize, k: usize, bw: usize, lv: usize, parent: Option<i64>) -> usize {
    let c = &b.c[band];
    let (i, j) = (k / bw, k % bw);
    let mut t = parent.map_or(0, |p| 2 * p.unsigned_abs());
    if j > 0 {
        t += c[k - 1].unsigned_abs();
    }
    if i > 0 {
        t += c[k - bw].unsigned_abs();
    }
    for o in 0..band {
        t += b.c[o][k].unsigned_abs();
    }
    (band * 3 + lv.min(2)) * BUCKETS + bucket(t)
}

/// The parent of block `k` (of a level of blocks `bw` wide) in the next level's band, whose
/// level is `up` and blocks `pb` (and None at the last level).
fn parent(k: usize, bw: usize, up: Option<(&Level, &Bands)>, band: usize) -> Option<i64> {
    let (l, b) = up?;
    let (pw, ph) = l.blocks();
    if pw == 0 || ph == 0 {
        return None;
    }
    let (i, j) = (k / bw, k % bw);
    let pi = (i.saturating_sub(l.py) / 2).min(ph - 1);
    let pj = (j.saturating_sub(l.px) / 2).min(pw - 1);
    Some(b.c[band][pi * pw + pj])
}

/// The border's values of a level, in order, each with the value it is predicted by
/// ([`border_pred`]).
fn border(v: &[i64], l: &Level, mut f: impl FnMut(usize, i64)) {
    for y in 0..l.h {
        for x in 0..l.w {
            if !l.in_blocks(x, y) {
                let mut pred = 0;
                border_pred(v, l, x, y, &mut pred);
                f(y * l.w + x, pred);
            }
        }
    }
}

/// The prediction of the border's value at `(x, y)`: the value below it if that is of a
/// block, else above, else left, else right if of a block, else 0.
#[inline]
fn border_pred(v: &[i64], l: &Level, x: usize, y: usize, pred: &mut i64) {
    let at = y * l.w + x;
    *pred = if y + 1 < l.h && l.in_blocks(x, y + 1) {
        v[at + l.w]
    } else if y > 0 {
        v[at - l.w]
    } else if x > 0 {
        v[at - 1]
    } else if x + 1 < l.w && l.in_blocks(x + 1, y) {
        v[at + 1]
    } else {
        0
    };
}

/// The cost of a level's bands at a phase, as the sum of the coefficients' bit lengths.
fn cost(v: &[i64], l: &Level) -> u64 {
    let (bw, bh) = l.blocks();
    let bits = |c: i64| 2 * (64 - zigzag(c).leading_zeros()) as u64 + 1;
    let mut sum = 0;
    for i in 0..bh {
        let r0 = (l.py + 2 * i) * l.w + l.px;
        let r1 = r0 + l.w;
        for j in 0..bw {
            let (a00, a01, a10, a11) = (v[r0 + 2 * j], v[r0 + 2 * j + 1], v[r1 + 2 * j], v[r1 + 2 * j + 1]);
            let hc = a11 - a10 - a01 + a00;
            let p = hc & 1;
            sum += bits(hc) + bits((a11 + a10 - a01 - a00 - p) >> 1) + bits((a11 - a10 + a01 - a00 - p) >> 1);
        }
    }
    sum
}

/// Codes the `w` × `h` values `v`.
pub fn encode(v: Vec<i64>, w: usize, h: usize) -> Coded {
    assert_eq!(v.len(), w * h);
    // the levels, from the finest: each its bands (without the next level's values) and the
    // differences of its border's values from their predictions
    let mut levels: Vec<(Level, Bands, Vec<i64>)> = Vec::new();
    let mut cur = v;
    let (mut cw, mut ch) = (w, h);
    while splits(cw, ch, levels.len()) {
        let l = (0..4).map(|p| Level { w: cw, h: ch, px: p & 1, py: p >> 1 }).min_by_key(|l| cost(&cur, l)).unwrap();
        let mut b = transform(&cur, &l);
        let mut edge = Vec::new();
        border(&cur, &l, |at, pred| edge.push(cur[at].wrapping_sub(pred)));
        cur = std::mem::take(&mut b.next);
        (cw, ch) = l.blocks();
        levels.push((l, b, edge));
    }
    let mut e = Encoder { m: Models::new(), ac: ArithEncoder::new(Vec::new()), raw: bits::Writer::default() };
    // the phases, from the finest level, then the last level's values, by their differences
    let mut ph = e.m.phase;
    for (l, _, _) in &levels {
        e.bit(l.px == 1, &mut ph[0]);
        e.bit(l.py == 1, &mut ph[1]);
    }
    let mut last = 0i64;
    let mut prev = 0u64;
    for &x in &cur {
        let d = x.wrapping_sub(last);
        e.put(TOP + bucket(prev), MANT_TOP, d);
        prev = d.unsigned_abs();
        last = x;
    }
    // the levels, from the coarsest: bands, border
    for lv in (0..levels.len()).rev() {
        let (l, b, edge) = &levels[lv];
        let up = levels.get(lv + 1).map(|(l, b, _)| (l, b));
        let (bw, bh) = l.blocks();
        for k in 0..bw * bh {
            for band in 0..3 {
                let ctx = coef_ctx(b, band, k, bw, lv, parent(k, bw, up, band));
                e.put(ctx, mant_ctx(band, lv), b.c[band][k]);
            }
        }
        let mut prev = 0u64;
        for &d in edge {
            e.put(BORDER + bucket(prev), MANT_BORDER, d);
            prev = d.unsigned_abs();
        }
    }
    Coded { model: e.ac.finish(), raw: e.raw.finish() }
}

/// The `w` × `h` values of [`encode`].
pub fn decode(c: &Coded, w: usize, h: usize) -> crate::Result<Vec<i64>> {
    let mut d = Decoder { m: Models::new(), ac: ArithDecoder::new(&c.model), raw: bits::Reader::new(&c.raw) };
    let mut levels: Vec<Level> = Vec::new();
    let (mut cw, mut ch) = (w, h);
    let mut ph = d.m.phase;
    while splits(cw, ch, levels.len()) {
        let px = d.bit(&mut ph[0]) as usize;
        let py = d.bit(&mut ph[1]) as usize;
        let l = Level { w: cw, h: ch, px, py };
        (cw, ch) = l.blocks();
        levels.push(l);
    }
    let mut cur = vec![0i64; cw * ch];
    let (mut last, mut prev) = (0i64, 0u64);
    for x in cur.iter_mut() {
        let dv = d.get(TOP + bucket(prev), MANT_TOP);
        prev = dv.unsigned_abs();
        *x = last.wrapping_add(dv);
        last = *x;
    }
    let mut above: Option<(Level, Bands)> = None;
    for lv in (0..levels.len()).rev() {
        let l = levels[lv];
        let (bw, bh) = l.blocks();
        let mut b = Bands { c: [vec![0; bw * bh], vec![0; bw * bh], vec![0; bw * bh]], next: cur };
        let up = above.as_ref().map(|(l, b)| (l, b));
        for k in 0..bw * bh {
            for band in 0..3 {
                let ctx = coef_ctx(&b, band, k, bw, lv, parent(k, bw, up, band));
                b.c[band][k] = d.get(ctx, mant_ctx(band, lv));
            }
        }
        let mut values = vec![0i64; l.w * l.h];
        inverse(&b, &l, &mut values);
        let mut prev = 0u64;
        let mut at_pred = Vec::new();
        border(&values, &l, |at, pred| at_pred.push((at, pred)));
        // (each border value's prediction is of a block or of a border value before it)
        for (at, _) in at_pred {
            let (x, y) = (at % l.w, at / l.w);
            let mut pred = 0;
            border_pred(&values, &l, x, y, &mut pred);
            let dv = d.get(BORDER + bucket(prev), MANT_BORDER);
            prev = dv.unsigned_abs();
            values[at] = pred.wrapping_add(dv);
        }
        above = Some((l, b));
        cur = values;
    }
    if d.ac.overrun() || d.raw.past_end() {
        return Err(crate::corrupt());
    }
    Ok(cur)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aqr_common::rng::Rng;

    #[test]
    fn round_trip() {
        let mut rng = Rng::new(9);
        for (w, h) in [(1, 1), (3, 7), (4, 4), (5, 9), (37, 23), (64, 64), (100, 3)] {
            for spread in [1u64, 30, 1 << 20, 1 << 40] {
                let v: Vec<i64> =
                    (0..w * h).map(|i| (i as i64 * 3) + rng.below(spread) as i64 - (spread / 2) as i64).collect();
                let c = encode(v.clone(), w, h);
                assert_eq!(decode(&c, w, h).unwrap(), v, "{w}x{h} {spread}");
            }
        }
    }
}
