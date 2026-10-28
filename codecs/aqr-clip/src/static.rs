//! The model of [`crate::dynamic`] (with `Escape::Med`, runs and tile kinds) in symbols of static
//! tables, coded with rANS a chunk at a time as aqr-psd's model is: what was a sequence of
//! adaptive bits is one symbol of a byte alphabet in a table chosen by the same contexts.
//!
//! Per tile, its kind (pixels, all zeros, one pixel). Per pixel outside runs, its mode: a copy
//! of the left, upper, upper right or upper left pixel (the first of them equal to it), its
//! place in the list, or new; in a table by whether it ends a run, the four equalities of
//! neighbours, the run of left copies and how the pixel above was coded. Then its place (a table), or its samples' residuals
//! (tables by sample, transparency, activity and G's residual). A run: whether as long as the
//! stretch above, else its length, in tables by whether in the first row, whether the stretch
//! reaches the end of the row and whether the last run was as long as its stretch.
//!
//! The encoder codes a listed pixel as new where that is cheaper by the counts of the chunk so
//! far (the decoder follows the mode).

use crate::tile::{AREA, Layout, SIDE};
use aqr_common::rans32;
use aqr_common::varint;

const PROB_BITS: u32 = 11;
type Tables = rans32::Tables<u8, PROB_BITS>;
type BitW = rans32::BitW<false>;
type BitR<'a> = rans32::BitR<'a, false>;

/// Symbols per chunk of the stream, each chunk with tables of its own.
pub const CHUNK: usize = 1 << 20;

const LIST: usize = 256;
const RUN_LOOK: usize = 4;
const KIND: usize = 0;
/// Modes: copies of the four neighbours, the list, new.
const MODE: usize = KIND + 1;
const MODE_CTX: usize = 2 * 16 * 4 * 4;
const M_LIST: u8 = 4;
const M_NEW: u8 = 5;
const PLACE: usize = MODE + MODE_CTX;
const SLOTS: usize = 5;
const ACT: usize = 8;
const CROSS: usize = 4;
const RESIDUAL: usize = PLACE + 1;
const RUN_CTX: usize = 8;
const FULL: usize = RESIDUAL + SLOTS * 2 * ACT * CROSS;
const LENGTH: usize = FULL + RUN_CTX;
pub const TABLES: usize = LENGTH + RUN_CTX;

pub trait Sym {
    const DECODES: bool;
    /// Codes symbol `s` (the encoder's) in table `t`, and gives the symbol coded.
    fn sym(&mut self, t: usize, s: u8) -> u8;
    /// About what symbol `s` of table `t` costs, in bits (the encoder's estimate).
    fn cost(&self, t: usize, s: u8) -> f32;
}

/// Records symbols and codes them a chunk at a time.
pub struct Recorder {
    syms: Vec<(u16, u16)>,
    counts: Vec<[u32; 256]>,
    totals: Vec<u32>,
    pub out: Vec<u8>,
    /// Bytes of the tables written, and of the rANS streams.
    pub head: usize,
    pub body: usize,
}

impl Default for Recorder {
    fn default() -> Self {
        Recorder {
            syms: Vec::with_capacity(CHUNK),
            counts: vec![[0; 256]; TABLES],
            totals: vec![0; TABLES],
            out: Vec::new(),
            head: 0,
            body: 0,
        }
    }
}

fn unused() -> Vec<u32> {
    let mut f = vec![0u32; 256];
    f[0] = 1 << PROB_BITS;
    f
}

impl Recorder {
    /// A chunk: symbol count, tables (a bit per table whether used, then its levels), rANS.
    fn flush(&mut self) {
        if self.syms.is_empty() {
            return;
        }
        let mut w = BitW::default();
        let mut freqs = Vec::with_capacity(TABLES);
        for c in &self.counts {
            let used = c.iter().any(|&n| n > 0);
            w.bits(1, used as u32);
            freqs.push(if used { rans32::write_table::<PROB_BITS, false>(&mut w, c) } else { unused() });
        }
        let head = w.finish();
        let body = rans32::encode::<PROB_BITS>(&self.syms, &freqs);
        self.head += head.len();
        self.body += body.len();
        varint::put(&mut self.out, self.syms.len() as u64);
        varint::put(&mut self.out, head.len() as u64);
        self.out.extend_from_slice(&head);
        varint::put(&mut self.out, body.len() as u64);
        self.out.extend_from_slice(&body);
        self.syms.clear();
        self.counts.iter_mut().for_each(|c| *c = [0; 256]);
        self.totals.iter_mut().for_each(|t| *t = 0);
    }

    /// The stream, and the bytes of its tables.
    pub fn finish(mut self) -> (Vec<u8>, usize) {
        self.flush();
        (self.out, self.head)
    }
}

impl Sym for Recorder {
    const DECODES: bool = false;
    #[inline(always)]
    fn sym(&mut self, t: usize, s: u8) -> u8 {
        if self.syms.len() == CHUNK {
            self.flush();
        }
        self.syms.push((t as u16, s as u16));
        self.counts[t][s as usize] += 1;
        self.totals[t] += 1;
        s
    }

    #[inline]
    fn cost(&self, t: usize, s: u8) -> f32 {
        ((self.totals[t] as f32 + 1.0) / (self.counts[t][s as usize] as f32 + 0.5)).log2()
    }
}

/// Decodes symbols chunk by chunk; a corrupt stream yields zeros and `failed`.
pub struct Player<'a> {
    src: &'a [u8],
    at: usize,
    tables: Tables,
    map: Vec<u16>,
    dec: rans32::Decoder<'a>,
    left: usize,
    pub failed: bool,
}

impl<'a> Player<'a> {
    pub fn new(src: &'a [u8]) -> Player<'a> {
        Player {
            src,
            at: 0,
            tables: Tables::new(&[]),
            map: vec![0; TABLES],
            dec: rans32::Decoder::new(&[]),
            left: 0,
            failed: false,
        }
    }

    fn next_chunk(&mut self) -> Option<()> {
        let src = self.src;
        let n = varint::get(src, &mut self.at).ok()? as usize;
        let hl = varint::get(src, &mut self.at).ok()? as usize;
        let head = src.get(self.at..self.at.checked_add(hl)?)?;
        self.at += hl;
        let mut r = BitR::new(head);
        let mut freqs = vec![unused()];
        for t in 0..TABLES {
            if r.bits(1) == 1 {
                self.map[t] = freqs.len() as u16;
                freqs.push(rans32::read_table::<PROB_BITS, false>(&mut r, 256).ok()?);
            } else {
                self.map[t] = 0;
            }
        }
        let bl = varint::get(src, &mut self.at).ok()? as usize;
        let body = src.get(self.at..self.at.checked_add(bl)?)?;
        self.at += bl;
        self.tables = Tables::new(&freqs);
        self.dec = rans32::Decoder::new(body);
        self.left = n;
        Some(())
    }

    /// Whether the stream was consumed exactly.
    pub fn finish(self) -> bool {
        !self.failed && self.left == 0 && self.dec.finished() && self.at == self.src.len()
    }
}

impl Sym for Player<'_> {
    const DECODES: bool = true;
    #[inline(always)]
    fn sym(&mut self, t: usize, _: u8) -> u8 {
        if self.left == 0 && (self.failed || self.at == self.src.len() || self.next_chunk().is_none()) {
            self.failed = true;
            return 0;
        }
        self.left -= 1;
        self.dec.get(&self.tables, self.map[t] as usize) as u8
    }

    fn cost(&self, _: usize, _: u8) -> f32 {
        0.0
    }
}

#[inline]
fn med(a: u8, b: u8, c: u8) -> u8 {
    let (lo, hi) = (a.min(b), a.max(b));
    if c >= hi {
        lo
    } else if c <= lo {
        hi
    } else {
        a.wrapping_add(b).wrapping_sub(c)
    }
}

#[inline]
fn bucket(v: u32, n: usize) -> usize {
    ((32 - v.leading_zeros()) as usize).min(n - 1)
}

/// Slots of the index of [`Ring`].
const HASH_BITS: u32 = 12;

/// The list: the last [`LIST`] pixels coded by their place or as new, newest at place 0. A
/// pixel coded again is pushed again (its older entry stays, unused). The encoder finds a
/// pixel's newest entry by a hash index (a pixel whose slot was taken over is not found, and so
/// coded as new); the decoder only reads places.
struct Ring {
    px: [u64; LIST],
    pushed: usize,
    index: Vec<u32>,
}

impl Ring {
    fn new() -> Ring {
        Ring { px: [0; LIST], pushed: 0, index: vec![0; 1 << HASH_BITS] }
    }

    #[inline]
    fn slot(v: u64) -> usize {
        (v.wrapping_mul(0x9e37_79b9_7f4a_7c15) >> (64 - HASH_BITS)) as usize
    }

    #[inline]
    fn push(&mut self, v: u64, indexed: bool) {
        self.px[self.pushed % LIST] = v;
        self.pushed += 1;
        if indexed {
            self.index[Self::slot(v)] = self.pushed as u32;
        }
    }

    #[inline]
    fn find(&self, v: u64) -> Option<usize> {
        let e = self.index[Self::slot(v)] as usize;
        (e > 0 && self.pushed - e < LIST && self.px[(e - 1) % LIST] == v).then(|| self.pushed - e)
    }

    #[inline]
    fn get(&self, place: usize) -> Option<u64> {
        (place < self.pushed.min(LIST)).then(|| self.px[(self.pushed - 1 - place) % LIST])
    }
}

pub struct Model {
    list: Ring,
    last_full: bool,
    /// Per pixel of the tile, how it was coded: 0 in a run or a copy of the left pixel, 1 another
    /// copy, 2 by its place in the list, 3 new.
    modes: Vec<u8>,
}

impl Default for Model {
    fn default() -> Self {
        Model { list: Ring::new(), last_full: false, modes: vec![0; AREA] }
    }
}

impl Model {
    /// The samples of a new pixel `want` (the encoder's) with neighbours left, upper, upper
    /// left and upper right, coded through `code` (a sample's table and zigzag residual in, the
    /// residual coded out).
    #[inline]
    fn fresh(l: Layout, want: u64, nb: [Option<u64>; 4], mut code: impl FnMut(usize, u8) -> u8) -> u64 {
        let [w, n, nw, ne] = nb;
        let (a, b, cc, d) = (w.or(n), n.or(w), nw.or(n).or(w), ne.or(n).or(w));
        let order: &[usize] = match l {
            Layout::Color => &[0, 2, 1, 3, 4],
            Layout::One => &[0],
            Layout::Two => &[0, 1],
        };
        let sample = |p: u64, s: usize| (p >> (8 * s)) as u8;
        let (mut v, mut transparent, mut g) = (0u64, false, None::<i32>);
        for (slot, &s) in order.iter().enumerate() {
            let at = |p: Option<u64>| p.map_or(0, |p| sample(p, s));
            let (a, b, cc, d) = (at(a), at(b), at(cc), at(d));
            let colour = l == Layout::Color && s > 0;
            let mut pred = med(a, b, cc);
            if let (true, Some(r), 1 | 3) = (colour, g, s) {
                pred = (pred as i32 + r).clamp(0, 255) as u8;
            }
            let act = (a as i32 - cc as i32).unsigned_abs()
                + (b as i32 - cc as i32).unsigned_abs()
                + (d as i32 - b as i32).unsigned_abs();
            let cross = match (s, g) {
                (1 | 3 | 4, Some(r)) => bucket(r.unsigned_abs(), CROSS),
                _ => 0,
            };
            let t = RESIDUAL + ((slot * 2 + (colour && transparent) as usize) * ACT + bucket(act, ACT)) * CROSS + cross;
            let r = sample(want, s).wrapping_sub(pred) as i8;
            let z = code(t, ((r as i32) << 1 ^ (r as i32) >> 7) as u8) as u32;
            let r = ((z >> 1) as i32 ^ -((z & 1) as i32)) as i8;
            let x = pred.wrapping_add(r as u8);
            if s == 0 && l == Layout::Color {
                transparent = x == 0;
            }
            if colour && s == 2 {
                g = Some(r as i32);
            }
            v |= (x as u64) << (8 * s);
        }
        v
    }

    /// Codes a tile of layout `l`: `px` holds the encoder's pixels, and the decoder's
    /// afterwards.
    pub fn tile<C: Sym>(&mut self, c: &mut C, l: Layout, px: &mut [u64]) {
        let first = px[0];
        let kind = if C::DECODES {
            0
        } else if px.iter().all(|&p| p == 0) {
            1
        } else if px.iter().all(|&p| p == first) {
            2
        } else {
            0
        };
        match c.sym(KIND, kind) {
            0 => {}
            1 => return px.fill(0),
            _ => {
                let v = Self::fresh(l, first, [None; 4], |t, z| c.sym(t, z));
                return px.fill(v);
            }
        }
        let (mut run, mut i, mut broke) = (0usize, 0, false);
        while i < AREA {
            let (x, y) = (i % SIDE, i / SIDE);
            if !broke && x > 0 {
                let w = px[i - 1];
                let left = SIDE - x;
                let above = if y > 0 {
                    px[i - SIDE..i - SIDE + left].iter().take_while(|&&p| p == w).count()
                } else if x > 1 && px[i - 2] == w {
                    left
                } else {
                    0
                };
                if above >= RUN_LOOK.min(left) && above > 0 {
                    let len = if C::DECODES { 0 } else { px[i..i + left].iter().take_while(|&&p| p == w).count() };
                    let t = (y > 0) as usize * 4 + (above == left) as usize * 2 + self.last_full as usize;
                    let full = c.sym(FULL + t, (len == above) as u8) == 1;
                    let len = if full { above } else { (c.sym(LENGTH + t, len as u8) as usize).min(left) };
                    self.last_full = full;
                    px[i..i + len].fill(w);
                    self.modes[i..i + len].fill(0);
                    i += len;
                    broke = len < left;
                    run = 0;
                    continue;
                }
            }
            let n = (y > 0).then(|| px[i - SIDE]);
            let w = if x > 0 { Some(px[i - 1]) } else { n };
            let nw = (x > 0 && y > 0).then(|| px[i - SIDE - 1]);
            let ne = (y > 0 && x + 1 < SIDE).then(|| px[i - SIDE + 1]);
            let eq = |a: Option<u64>, b: Option<u64>| a.is_some() && a == b;
            let r = match run {
                0 => 0,
                1..=2 => 1,
                3..=15 => 2,
                _ => 3,
            };
            let up = if y > 0 { self.modes[i - SIDE] as usize } else { 0 };
            let ctx = MODE
                + ((broke as usize * 16
                    + (eq(w, n) as usize
                        | (eq(n, ne) as usize) << 1
                        | (eq(w, nw) as usize) << 2
                        | (eq(nw, n) as usize) << 3))
                    * 4
                    + r)
                    * 4
                + up;
            // the candidates: each distinct value once, the left one not after a run
            let mut cands = [None; 4];
            let mut seen = [0u64; 5];
            let mut ns = 0;
            if broke {
                seen[0] = px[i - 1];
                ns = 1;
            }
            for (k, cand) in [w, n, ne, nw].into_iter().enumerate() {
                if let Some(v) = cand
                    && !seen[..ns].contains(&v)
                {
                    seen[ns] = v;
                    ns += 1;
                    cands[k] = Some(v);
                }
            }
            broke = false;
            let want = px[i];
            let nb = [w, n, nw, ne];
            let mut mode = M_NEW;
            let mut at = None;
            if !C::DECODES {
                if let Some(k) = cands.iter().position(|&v| v == Some(want)) {
                    mode = k as u8;
                } else if let Some(j) = self.list.find(want) {
                    let by_place = c.cost(ctx, M_LIST) + c.cost(PLACE, j as u8);
                    let mut by_samples = c.cost(ctx, M_NEW);
                    Self::fresh(l, want, nb, |t, z| {
                        by_samples += c.cost(t, z);
                        z
                    });
                    if by_place <= by_samples {
                        mode = M_LIST;
                        at = Some(j);
                    }
                }
            }
            let v = match c.sym(ctx, mode) {
                k @ 0..4 => {
                    run = if k == 0 { run + 1 } else { 0 };
                    self.modes[i] = (k != 0) as u8;
                    // (a corrupt stream may name a missing candidate)
                    cands[k as usize].unwrap_or(0)
                }
                M_LIST => {
                    run = 0;
                    self.modes[i] = 2;
                    // (a corrupt place stands for 0)
                    let v = self.list.get(c.sym(PLACE, at.unwrap_or(0) as u8) as usize).unwrap_or(0);
                    self.list.push(v, !C::DECODES);
                    v
                }
                _ => {
                    run = 0;
                    self.modes[i] = 3;
                    let v = Self::fresh(l, want, nb, |t, z| c.sym(t, z));
                    self.list.push(v, !C::DECODES);
                    v
                }
            };
            px[i] = v;
            i += 1;
        }
    }
}
