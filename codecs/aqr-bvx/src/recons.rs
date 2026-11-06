//! The record of a stream given its data (`FORMAT.md` §6): its segments and blocks, the parse
//! of each segment under contexts of what a match finder sees in the data, and what LZFSE
//! blocks hold beyond their usual choices.
//!
//! In LZFSE segments, a match is coded first as one of the two that the compressor's own table
//! offers there (it nearly always is); else, as LZVN's are, as which of the nearest matches of
//! increasing lengths it is, its distance not at all where it is the nearest for its length.

use aqr_common::arith::{ArithDecoder, ArithEncoder, Bit};

use crate::bvx::{self, D_MAX, Freqs};
use crate::plan::{self, Counts, Match, Part, Plan, Side};
use crate::{Error, Result, corrupt};

pub use aqr_common::lz::{lcp, lcp_up};

/// The triples of a full LZFSE block, as the compressor cuts them.
pub const BLOCK_LMD: u32 = 9992;

/// Candidates looked at for the nearest matches of increasing lengths.
pub const DEPTH: usize = 32;
/// Candidates looked at for the nearest match of a length beyond those.
pub const LIMIT: usize = 1024;
/// Positions at either end of a long match that the match finder holds.
pub const EDGE: usize = 32;
/// Candidates whose longest match is a context of whether a match starts.
pub const NEAR: usize = 4;

const NONE: u32 = u32::MAX;
const RING_BITS: u32 = 18;
const RING: usize = 1 << RING_BITS;
const HEAD_BITS: u32 = 18;

/// What the matches of a kind of segment are.
#[derive(Clone, Copy)]
struct Kind {
    /// The shortest match the match finder offers, and the farthest.
    min: usize,
    window: usize,
}

const LZFSE: Kind = Kind { min: 4, window: D_MAX as usize };
const LZVN: Kind = Kind { min: 3, window: 65535 };

/// Bucket of a run of literals: its bit length, at most 7.
#[inline]
fn bucket(x: u32) -> usize {
    ((u32::BITS - x.leading_zeros()) as usize).min(7)
}

/// Bucket of a length: itself below 8, else 8-11 by its bit length.
#[inline]
fn lb(l: usize) -> usize {
    if l < 8 { l } else { (5 + (usize::BITS - l.leading_zeros()) as usize).min(11) }
}

/// The match finder: every position before the current one on a chain of the hash of its first
/// `min` bytes. Candidates are those whose first `min` bytes are the current position's.
struct Mf<'a> {
    d: &'a [u8],
    k: Kind,
    /// Of `1 << HEAD_BITS` and [`RING`] entries.
    head: &'a mut [u32],
    prev: &'a mut [u32],
    /// Positions below this are inserted.
    next: usize,
}

impl<'a> Mf<'a> {
    fn new(d: &'a [u8], k: Kind, head: &'a mut [u32], prev: &'a mut [u32]) -> Self {
        head.fill(NONE);
        prev.fill(NONE);
        Mf { d, k, head, prev, next: 0 }
    }

    #[inline]
    fn key(&self, p: usize) -> u32 {
        let d = self.d;
        if self.k.min == 4 {
            u32::from_le_bytes(d[p..p + 4].try_into().unwrap())
        } else {
            d[p] as u32 | (d[p + 1] as u32) << 8 | (d[p + 2] as u32) << 16
        }
    }

    #[inline]
    fn hash(x: u32) -> usize {
        (x.wrapping_mul(0x9e37_79b1) >> (32 - HEAD_BITS)) as usize
    }

    /// Inserts the positions up to `pos`.
    #[inline]
    fn advance(&mut self, pos: usize) {
        while self.next < pos {
            let p = self.next;
            if p + self.k.min <= self.d.len() {
                let h = Self::hash(self.key(p));
                self.prev[p % RING] = self.head[h];
                self.head[h] = p as u32;
            }
            self.next += 1;
        }
    }

    /// Leaves the middle of a long match at `pos` of `n` bytes out: only its first and last
    /// [`EDGE`] positions are inserted.
    #[inline]
    fn skip(&mut self, pos: usize, n: usize) {
        if n > 2 * EDGE {
            self.advance(pos + EDGE);
            self.next = self.next.max(pos + n - EDGE);
        }
    }

    /// The candidates at `pos`, nearest first, given to `f` with their rank until it returns
    /// false, at most `n` of them.
    #[inline]
    fn each(&self, pos: usize, n: usize, mut f: impl FnMut(usize, usize) -> bool) {
        if pos + self.k.min > self.d.len() {
            return;
        }
        let key = self.key(pos);
        let mut c = self.head[Self::hash(key)];
        // (`pos` itself is on the chain once the next position's candidates are looked at)
        if c as usize == pos {
            c = self.prev[pos % RING];
        }
        let (mut rank, mut seen) = (0, 0);
        // (a chain holds others of the same hash, which are skipped but bound the walk)
        while c != NONE && rank < n && seen < 4 * n + 64 {
            let p = c as usize;
            if pos - p > self.k.window {
                break;
            }
            seen += 1;
            if self.key(p) == key {
                if !f(p, rank) {
                    return;
                }
                rank += 1;
            }
            c = self.prev[p % RING];
            if c != NONE && c as usize >= p {
                break;
            }
        }
    }

    /// The longest match among the first [`NEAR`] candidates at `pos` (0 for none).
    fn near(&self, pos: usize) -> usize {
        let mut best = 0;
        let d = self.d;
        self.each(pos, NEAR, |p, _| {
            if d.get(p + best) == d.get(pos + best) {
                best = best.max(lcp(d, p, pos));
            }
            true
        });
        best
    }

    /// The nearest matches at `pos` of increasing lengths, among the first [`DEPTH`]
    /// candidates: (length, position, rank).
    fn pairs(&self, pos: usize, out: &mut Vec<(usize, usize, usize)>) {
        let d = self.d;
        let cap = d.len() - pos;
        out.clear();
        let mut t = 0;
        self.each(pos, DEPTH, |p, rank| {
            if d[p + t] == d[pos + t] {
                let n = lcp(d, p, pos);
                if n > t {
                    t = n;
                    out.push((n, p, rank));
                }
            }
            t < cap
        });
    }

    /// The nearest match of at least `n` bytes at `pos`, among the first [`LIMIT`] candidates.
    fn nearest(&self, pos: usize, n: usize) -> Option<usize> {
        let d = self.d;
        let mut found = None;
        self.each(pos, LIMIT, |p, _| {
            if d[p + n - 1] == d[pos + n - 1] && lcp(d, p, pos) >= n {
                found = Some(p);
                return false;
            }
            true
        });
        found
    }
}

/// The LZFSE compressor's table of candidates (`FORMAT.md` §6.3): buckets of the latest
/// [`WAYS`] positions by a hash of their first 4 bytes, every position inserted in turn. A
/// position is looked up before it is inserted.
struct Table<'a> {
    d: &'a [u8],
    /// Of `(1 << TABLE_BITS) * WAYS` entries.
    tab: &'a mut [u32],
    /// Positions below this are inserted.
    next: usize,
    /// The last positions looked up, with their candidates.
    looked: [(usize, [u32; WAYS]); 4],
}

/// A match a table offers: length (0 for none) and distance.
type Cand = (usize, usize);

pub const TABLE_BITS: u32 = 14;
pub const WAYS: usize = 4;
/// Bytes of the table.
pub const TABLE_BYTES: u64 = ((1 << TABLE_BITS) * WAYS * 4) as u64;

/// The match finder's tables and the table of candidates, made once for all the segments.
struct Tables {
    head: Vec<u32>,
    prev: Vec<u32>,
    tab: Vec<u32>,
    pairs: Vec<(usize, usize, usize)>,
}

/// Bytes of [`Tables`] (the pairs of 12 bytes on WebAssembly).
pub const TABLES_HEAP: [u64; 4] = [(1 << HEAD_BITS) * 4, (RING * 4) as u64, TABLE_BYTES, 12 * (DEPTH as u64 + 1)];

impl Tables {
    fn new() -> Tables {
        Tables {
            head: vec![NONE; 1 << HEAD_BITS],
            prev: vec![NONE; RING],
            tab: vec![NONE; (1 << TABLE_BITS) * WAYS],
            pairs: Vec::with_capacity(DEPTH + 1),
        }
    }
}

/// The longer, or the nearer of two as long.
#[inline]
fn better(a: Cand, b: Cand) -> Cand {
    if b.0 > a.0 || b.0 == a.0 && b.0 > 0 && b.1 < a.1 { b } else { a }
}

impl<'a> Table<'a> {
    fn new(d: &'a [u8], tab: &'a mut [u32]) -> Self {
        tab.fill(NONE);
        Table { d, tab, next: 0, looked: [(usize::MAX, [NONE; WAYS]); 4] }
    }

    #[inline]
    fn bucket(&self, p: usize) -> usize {
        let x = u32::from_le_bytes(self.d[p..p + 4].try_into().unwrap());
        (x.wrapping_mul(0x9e37_79b1) >> (32 - TABLE_BITS)) as usize * WAYS
    }

    #[inline]
    fn insert(&mut self, p: usize) {
        if p + 4 <= self.d.len() {
            let b = self.bucket(p);
            // (by hand: copy_within is a call of memmove)
            let e: &mut [u32; WAYS] = (&mut self.tab[b..b + WAYS]).try_into().unwrap();
            *e = [p as u32, e[0], e[1], e[2]];
        }
    }

    /// Inserts the positions up to `p`. In a run of a byte, whose positions are all of one
    /// bucket, only the last [`WAYS`] are, as the others would be pushed out.
    fn fill(&mut self, p: usize) {
        let d = self.d;
        while self.next < p {
            let q = self.next;
            if q + 8 <= p && d[q] == d[q + 1] && d[q..q + 5].iter().all(|&x| x == d[q]) {
                // the run's positions of the same first 4 bytes: up to its end less 3
                let end = q + d[q..p.min(d.len())].iter().position(|&x| x != d[q]).unwrap_or(p.min(d.len()) - q);
                let last = (end - 3).min(p);
                if last > q + WAYS {
                    self.next = last - WAYS;
                }
                while self.next < last {
                    self.insert(self.next);
                    self.next += 1;
                }
                continue;
            }
            self.insert(q);
            self.next += 1;
        }
    }

    /// The candidates of `p` of its bucket (the same first 4 bytes, in reach; positions before
    /// it inserted without a look), `p` inserted after.
    fn at(&mut self, p: usize) -> [u32; WAYS] {
        if let Some(&(_, c)) = self.looked.iter().find(|l| l.0 == p) {
            return c;
        }
        self.fill(p);
        let d = self.d;
        let mut c = [NONE; WAYS];
        if p + 4 <= d.len() && self.next == p {
            let key = &d[p..p + 4];
            let k = self.bucket(p);
            for (c, &q) in c.iter_mut().zip(&self.tab[k..k + WAYS]) {
                if q != NONE && p - q as usize <= D_MAX as usize && d[q as usize..q as usize + 4] == *key {
                    *c = q;
                }
            }
            self.insert(p);
            self.next = p + 1;
        }
        self.looked.rotate_right(1);
        self.looked[0] = (p, c);
        c
    }

    /// The matches at `p`, their lengths up to `cap`: the best of its bucket (`A`), and the best
    /// of the next position's that starts at `p` (`B`).
    fn pair(&mut self, p: usize, cap: usize) -> (Cand, Cand) {
        let d = self.d;
        let mut a = (0, 0);
        for q in self.at(p).into_iter().filter(|&q| q != NONE).map(|q| q as usize) {
            a = better(a, (lcp_up(d, q, p, cap), p - q));
        }
        let mut b = (0, 0);
        if p + 1 < d.len() {
            for q in self.at(p + 1).into_iter().filter(|&q| q != NONE).map(|q| q as usize) {
                if q > 0 && d[q - 1] == d[p] {
                    b = better(b, ((1 + lcp_up(d, q, p + 1, cap)).min(cap), p + 1 - q));
                }
            }
        }
        (a, b)
    }
}

/// The table's guesses at a position: the first and the second, and the class of their lengths.
fn guesses((a, b): (Cand, Cand)) -> (Cand, Cand, usize) {
    let class = match (a.0, b.0) {
        (0, 0) => 0,
        (0, _) => 1,
        (_, 0) => 2,
        (x, y) if y <= x => 3,
        (x, y) => 3 + (y - x).min(3),
    };
    // (the longer first; `B` must be longer by 2 to be the first)
    if b.0 > a.0 + 1 { (b, a, class) } else { (a, b, class) }
}

/// What the record codes, for the statistics.
#[derive(Clone, Copy)]
#[repr(usize)]
pub enum Cat {
    /// The segments and blocks, and the LZFSE blocks' sides.
    Layout,
    /// Whether a match starts.
    Start,
    Len,
    Dist,
}
pub const CATS: usize = 4;

/// The contexts of a kind of segment's parses.
struct Walk {
    start: [Bit; 8 * 12 * 3],
    take: [Bit; 4 * 9],
    longer: [Bit; 64],
    full: [Bit; 12],
    short: [[Bit; 64]; 12],
    far: [Bit; 12],
    dist_bits: [Bit; 64],
    /// Whether the match is the table's first guess, or its second.
    best: [Bit; 7 * 12],
    second: [Bit; 7 * 12],
}

impl Walk {
    fn new() -> Self {
        let b = Bit::default();
        Walk {
            start: [b; 8 * 12 * 3],
            take: [b; 4 * 9],
            longer: [b; 64],
            full: [b; 12],
            short: [[b; 64]; 12],
            far: [b; 12],
            dist_bits: [b; 64],
            best: [b; 7 * 12],
            second: [b; 7 * 12],
        }
    }
}

struct Ctx {
    walks: [Walk; 2],
    kind: [Bit; 2],
    rest: Bit,
    len: [Bit; 64],
    full_block: Bit,
    block_len: [Bit; 64],
    usual: Bit,
    freqs: Bit,
    value: [Bit; 64],
}

impl Ctx {
    fn new() -> Self {
        let g = || [Bit::default(); 64];
        Ctx {
            walks: [Walk::new(), Walk::new()],
            kind: [Bit::default(); 2],
            rest: Bit::default(),
            len: g(),
            full_block: Bit::default(),
            block_len: g(),
            usual: Bit::default(),
            freqs: Bit::default(),
            value: g(),
        }
    }
}

/// The side of the record's coder.
trait Coder {
    fn bit(&mut self, b: &mut Bit, v: bool, cat: Cat) -> bool;

    /// `n` bits of probability 1/2.
    fn raw(&mut self, n: u32, v: u32, cat: Cat) -> u32;

    /// `v < 2^32 - 1` as Elias gamma of `v + 1`: its bit length less 1 in unary under the
    /// adaptive bits `t` (the last one shared beyond), then the bits below the top one, raw.
    fn gamma(&mut self, t: &mut [Bit], v: u32, cat: Cat) -> Result<u32>;
}

/// The encoder, counting the bits of each [`Cat`].
type Enc = aqr_common::arith::Costed<CATS>;

/// Each side of `aqr_common::arith`'s coder codes the record, each bit of a [`Cat`].
impl<C: aqr_common::arith::Coder> Coder for C {
    #[inline]
    fn bit(&mut self, b: &mut Bit, v: bool, cat: Cat) -> bool {
        aqr_common::arith::Coder::set_cat(self, cat as usize);
        aqr_common::arith::Coder::bit(self, b, v)
    }

    #[inline]
    fn raw(&mut self, n: u32, v: u32, cat: Cat) -> u32 {
        aqr_common::arith::Coder::set_cat(self, cat as usize);
        aqr_common::arith::Coder::raws(self, n, v as u64) as u32
    }

    fn gamma(&mut self, t: &mut [Bit], v: u32, cat: Cat) -> Result<u32> {
        aqr_common::arith::Coder::set_cat(self, cat as usize);
        aqr_common::arith::Coder::gamma32(self, t, v, corrupt)
    }
}

/// Pushes `x` onto `v` within its capacity.
#[inline]
fn push<T>(v: &mut Vec<T>, x: T) -> Result<()> {
    if v.len() == v.capacity() {
        return Err(corrupt());
    }
    v.push(x);
    Ok(())
}

/// Codes the parse of the segment `d[start..]`, the positions before `start` being in reach:
/// its matches (`want`'s where encoding) pushed onto `out` (within its capacity), and its
/// literals after them returned.
#[allow(clippy::too_many_arguments)]
fn walk(
    c: &mut impl Coder,
    w: &mut Walk,
    d: &[u8],
    start: usize,
    k: Kind,
    t: &mut Tables,
    want: Option<(&[Match], u32)>,
    out: &mut Vec<Match>,
) -> Result<u32> {
    let encoding = want.is_some();
    let mut mf = Mf::new(d, k, &mut t.head, &mut t.prev);
    mf.advance(start);
    let mut table = (k.min == 4).then(|| Table::new(d, &mut t.tab));
    let pairs = &mut t.pairs;
    let (mut pos, mut run) = (start, 0u32);
    let mut next = 0;
    // the longest of the near candidates at `pos`, once known
    let mut here: Option<usize> = None;
    while pos < d.len() {
        mf.advance(pos);
        let (l0, l1, g) = if let Some(t) = &mut table {
            let g = guesses(t.pair(pos, usize::MAX));
            mf.advance(pos + 1);
            // (the next position's only as a context, of lengths up to 64 told apart)
            (g.0.0.max(g.1.0), if pos + 1 < d.len() { guesses(t.pair(pos + 1, 64)).0.0 } else { 0 }, g)
        } else {
            let l0 = match here.take() {
                Some(l) => l,
                None => mf.near(pos),
            };
            mf.advance(pos + 1);
            let l1 = if pos + 1 < d.len() { mf.near(pos + 1) } else { 0 };
            here = Some(l1);
            (l0, l1, ((0, 0), (0, 0), 0))
        };
        let want = want.and_then(|(ms, _)| ms.get(next));
        let starts = want.is_some_and(|m| m.lits == run);
        let cmp = if l1 > l0 { 2 } else { (l1 == l0) as usize };
        if !c.bit(&mut w.start[(bucket(run) * 12 + lb(l0)) * 3 + cmp], starts, Cat::Start) {
            pos += 1;
            run += 1;
            continue;
        }
        here = None;
        let want = want.copied().unwrap_or(Match { lits: run, len: 0, dist: 0 });
        let (len, dist) = (want.len as usize, want.dist as usize);
        let (g0, g1, class) = g;
        let mut guess = None;
        if g0.0 > 0 {
            if c.bit(&mut w.best[class * 12 + lb(g0.0)], (len, dist) == g0, Cat::Len) {
                guess = Some(g0);
            } else if g1.0 > 0 && g1 != g0 && c.bit(&mut w.second[class * 12 + lb(g1.0)], (len, dist) == g1, Cat::Len) {
                guess = Some(g1);
            }
        }
        if let Some((n, dist)) = guess {
            let got = Match { lits: run, len: n as u32, dist: dist as u32 };
            if encoding && got != want {
                return Err(Error("match not coded"));
            }
            push(out, got)?;
            next += 1;
            mf.skip(pos, n);
            pos += n;
            run = 0;
            continue;
        }
        mf.pairs(pos, pairs);
        let m = pairs.len();
        // which pair: the first, from the nearest, whose length is at least the match's
        let j_want = if encoding { pairs.iter().take_while(|&&(l, _, _)| l < len).count() } else { 0 };
        let mut j = 0;
        while j < m {
            let next_rank = pairs.get(j + 1).map_or(8, |p| p.2.min(7));
            if c.bit(&mut w.take[j.min(3) * 9 + next_rank], j_want == j, Cat::Len) {
                break;
            }
            j += 1;
        }
        let (n, near) = if j == m {
            let top = pairs.last().map_or(0, |p| p.0);
            let v = (len.wrapping_sub(top + 1)) as u32;
            let n = top + 1 + c.gamma(&mut w.longer, v, Cat::Len)? as usize;
            if n > d.len() - pos {
                return Err(corrupt());
            }
            (n, if n >= k.min { mf.nearest(pos, n).map(|p| pos - p) } else { None })
        } else {
            let (top, p, _) = pairs[j];
            let lo = if j > 0 { pairs[j - 1].0 + 1 } else { 1 };
            let n = if c.bit(&mut w.full[lb(top)], len == top, Cat::Len) {
                top
            } else {
                let v = (top.wrapping_sub(1).wrapping_sub(len)) as u32;
                let s = c.gamma(&mut w.short[lb(top)], v, Cat::Len)? as usize;
                top.checked_sub(1 + s).filter(|&n| n >= lo).ok_or_else(corrupt)?
            };
            (n, Some(pos - p))
        };
        let far = match near {
            Some(nd) => c.bit(&mut w.far[lb(n)], dist != nd, Cat::Dist),
            None => true,
        };
        let dist = if far {
            let b = usize::BITS - dist.leading_zeros();
            let mut m = 1;
            for i in (0..5).rev() {
                m = m << 1 | c.bit(&mut w.dist_bits[m], b >> i & 1 != 0, Cat::Dist) as usize;
            }
            let b = m as u32 - 32;
            if !(1..=18).contains(&b) {
                return Err(corrupt());
            }
            (1 << (b - 1) | c.raw(b - 1, dist as u32, Cat::Dist)) as usize
        } else {
            near.unwrap()
        };
        if dist > pos.min(k.window) || n > d.len() - pos || lcp(d, pos - dist, pos) < n {
            return Err(corrupt());
        }
        let got = Match { lits: run, len: n as u32, dist: dist as u32 };
        if encoding && got != want {
            return Err(Error("match not coded"));
        }
        push(out, got)?;
        next += 1;
        mf.skip(pos, n);
        pos += n;
        run = 0;
    }
    if want.is_some_and(|(ms, tail)| next != ms.len() || run != tail) {
        return Err(Error("parse beyond the data"));
    }
    Ok(run)
}

/// Codes the plan over `data`: `want`'s where encoding, pushed onto `out` (within its room;
/// but for the LZFSE blocks' pads and distances of no match, [`code_fill`]).
fn code(c: &mut impl Coder, data: &[u8], want: Option<&Plan>, out: &mut Plan) -> Result<()> {
    let encoding = want.is_some();
    let empty = Plan::default();
    let inp = want.unwrap_or(&empty);
    let mut ctx = Ctx::new();
    let mut tables = Tables::new();
    let (mut pi, mut si, mut fi) = (0, 0, 0);
    let mut at = 0usize;
    // (no data: no block, or a stored block of nothing, as the compressor writes it)
    if data.is_empty() && c.bit(&mut ctx.rest, inp.parts == [Part::Stored(0)], Cat::Layout) {
        push(&mut out.parts, Part::Stored(0))?;
        pi = 1;
    }
    while at < data.len() {
        // the kind: LZFSE, LZVN or stored; and the bytes of data, the rest or fewer
        let (kind, want_len, want_end) = if encoding {
            let p = *inp.parts.get(pi).ok_or_else(corrupt)?;
            let end = pi + inp.parts[pi..].iter().take_while(|p| matches!(p, Part::Lzfse(..))).count();
            match p {
                Part::Stored(n) => (2, n as usize, pi + 1),
                Part::Lzvn(n) => (1, n as usize, pi + 1),
                Part::Lzfse(..) => {
                    let (ms, tail) = inp.seg(si)?;
                    (0, plan::seg_len(ms, tail) as usize, end)
                }
            }
        } else {
            (0, 0, 0)
        };
        let k = if c.bit(&mut ctx.kind[0], kind != 0, Cat::Layout) {
            1 + c.bit(&mut ctx.kind[1], kind == 2, Cat::Layout) as usize
        } else {
            0
        };
        let left = data.len() - at;
        let n = if c.bit(&mut ctx.rest, want_len == left, Cat::Layout) {
            left
        } else {
            let n = 1 + c.gamma(&mut ctx.len, want_len.wrapping_sub(1) as u32, Cat::Layout)? as usize;
            if n >= left {
                return Err(corrupt());
            }
            n
        };
        let d = &data[..at + n];
        let want_seg = if encoding && k != 2 { Some(inp.seg(si)?) } else { None };
        match k {
            2 => {
                if n > u32::MAX as usize {
                    return Err(corrupt());
                }
                push(&mut out.parts, Part::Stored(n as u32))?;
                pi += 1;
            }
            1 => {
                let tail = walk(c, &mut ctx.walks[1], d, at, LZVN, &mut tables, want_seg, &mut out.matches)?;
                push(&mut out.parts, Part::Lzvn(n as u32))?;
                push(&mut out.segs, plan::Seg { end: out.matches.len() as u32, tail })?;
                pi += 1;
                si += 1;
            }
            _ => {
                let from = out.matches.len();
                let tail = walk(c, &mut ctx.walks[0], d, at, LZFSE, &mut tables, want_seg, &mut out.matches)?;
                // the blocks: their triples, of the full count but for the last
                let total = plan::triples(&out.matches[from..], tail).count() as u32;
                let (mut done, mut i) = (0, pi);
                while done < total {
                    let (want, want_side) = if encoding {
                        match inp.parts.get(i) {
                            Some(&Part::Lzfse(n, s)) => (n, s),
                            _ => return Err(corrupt()),
                        }
                    } else {
                        (0, Side::USUAL)
                    };
                    let usual = BLOCK_LMD.min(total - done);
                    let n = if c.bit(&mut ctx.full_block, want == usual, Cat::Layout) {
                        usual
                    } else {
                        let n = 1 + c.gamma(&mut ctx.block_len, want.wrapping_sub(1), Cat::Layout)?;
                        if n > total - done || n == usual {
                            return Err(corrupt());
                        }
                        n
                    };
                    let mut side = want_side;
                    let want_freqs = if encoding && side.own {
                        fi += 1;
                        Some(inp.freqs.get(fi - 1).ok_or_else(corrupt)?)
                    } else {
                        None
                    };
                    code_side(c, &mut ctx, &mut side, want_freqs, &mut out.freqs, encoding)?;
                    push(&mut out.parts, Part::Lzfse(n, side))?;
                    done += n;
                    i += 1;
                }
                if encoding && i != want_end {
                    return Err(Error("blocks not coded"));
                }
                pi = i;
                push(&mut out.segs, plan::Seg { end: out.matches.len() as u32, tail })?;
                si += 1;
            }
        }
        at += n;
    }
    if encoding && (pi != inp.parts.len() || si != inp.segs.len()) {
        return Err(Error("plan beyond the data"));
    }
    Ok(())
}

/// Codes what an LZFSE block holds beyond its usual choices (its own frequencies, `want`'s
/// where encoding, pushed onto `freqs`), but for its pad and distances of no match, whose counts
/// are known only once all the blocks are ([`code_fill`]).
fn code_side(
    c: &mut impl Coder,
    ctx: &mut Ctx,
    side: &mut Side,
    want: Option<&bvx::Freqs>,
    freqs: &mut Vec<Freqs>,
    encoding: bool,
) -> Result<()> {
    if !c.bit(&mut ctx.usual, side.usual(), Cat::Layout) {
        side.own = c.bit(&mut ctx.freqs, side.own, Cat::Layout);
        if side.own {
            let mut f = want.cloned().unwrap_or(Freqs { l: [0; 20], m: [0; 20], d: [0; 64], lit: [0; 256] });
            for v in f.l.iter_mut().chain(&mut f.m).chain(&mut f.d).chain(&mut f.lit) {
                let x = c.gamma(&mut ctx.value, *v as u32, Cat::Layout)?;
                *v = u16::try_from(x).map_err(|_| corrupt())?;
            }
            push(freqs, f)?;
        }
        for (s, n) in side.lit_start.iter_mut().map(|s| (s, 10)).chain(side.lmd_start.iter_mut().map(|s| (s, 10))) {
            *s = c.raw(n, *s, Cat::Layout);
        }
        side.lit_low = c.gamma(&mut ctx.value, side.lit_low, Cat::Layout)?;
        side.lmd_low = c.gamma(&mut ctx.value, side.lmd_low, Cat::Layout)?;
        if !encoding && side.usual() {
            return Err(corrupt());
        }
    } else if !encoding {
        *side = Side::USUAL;
    }
    Ok(())
}

/// Codes the pad and the distances of no match of each LZFSE block of `out` (pushed onto its
/// `zd`; `want`'s where encoding), once its triples are known.
fn code_fill(c: &mut impl Coder, want: Option<&Plan>, out: &mut Plan) -> Result<()> {
    let mut ctx_pad = Bit::default();
    let mut ctx_zd = Bit::default();
    let mut zd_bits = [Bit::default(); 64];
    let encoding = want.is_some();
    let mut want_zd = want.map(|p| p.zd.iter());
    let Plan { parts, segs, matches, zd, .. } = out;
    let (mut i, mut si) = (0, 0);
    while i < parts.len() {
        match parts[i] {
            Part::Stored(_) => {
                i += 1;
                continue;
            }
            Part::Lzvn(_) => {
                (i, si) = (i + 1, si + 1);
                continue;
            }
            Part::Lzfse(..) => {}
        }
        let start = if si == 0 { 0 } else { segs[si - 1].end as usize };
        let seg = segs.get(si).ok_or_else(corrupt)?;
        let mut ts = plan::triples(matches.get(start..seg.end as usize).ok_or_else(corrupt)?, seg.tail);
        si += 1;
        while let Some(Part::Lzfse(n, side)) = parts.get_mut(i) {
            let (mut lits, mut none) = (0, 0);
            for _ in 0..*n {
                let t = ts.next().ok_or_else(corrupt)?;
                lits += t.0 as usize;
                none += t.2.is_none() as usize;
            }
            let npad = (4 - lits % 4) % 4;
            if encoding && side.npad as usize != npad {
                return Err(Error("side of another shape"));
            }
            side.npad = npad as u8;
            for b in side.pad[..npad].iter_mut() {
                if !c.bit(&mut ctx_pad, *b == 0, Cat::Layout) {
                    *b = c.raw(8, *b as u32, Cat::Layout) as u8;
                } else {
                    *b = 0;
                }
            }
            for _ in 0..none {
                let d = match &mut want_zd {
                    Some(z) => *z.next().ok_or(Error("side of another shape"))?,
                    None => 0,
                };
                let d = if !c.bit(&mut ctx_zd, d == 0, Cat::Layout) {
                    1 + c.gamma(&mut zd_bits, d.wrapping_sub(1), Cat::Layout)?
                } else {
                    0
                };
                push(zd, d)?;
            }
            i += 1;
        }
    }
    if want_zd.is_some_and(|mut z| z.next().is_some()) {
        return Err(Error("side of another shape"));
    }
    Ok(())
}

/// A stream split.
pub struct Split {
    pub data: Vec<u8>,
    pub record: Vec<u8>,
    /// The plan's counts: the module's params.
    pub counts: Counts,
    /// Bytes of the stream, up to its end.
    pub used: usize,
    /// Bits of the record by [`Cat`].
    pub cost: [f64; CATS],
}

/// The record of `plan` over `data`.
pub fn record(data: &[u8], plan: &Plan) -> Result<(Vec<u8>, [f64; CATS])> {
    let mut e = Enc::with(ArithEncoder::new(Vec::new()));
    let mut p = Plan::with_room(&plan.counts()?);
    code(&mut e, data, Some(plan), &mut p)?;
    code_fill(&mut e, Some(plan), &mut p)?;
    if p != *plan {
        return Err(Error("plan not coded"));
    }
    Ok((e.c.finish(), e.cost))
}

/// Splits the stream at the start of `s` into its data (at most `limit` bytes) and record.
pub fn split(s: &[u8], limit: usize) -> Result<Split> {
    let st = bvx::decode(s, limit)?;
    let plan = plan::split(&st, s)?;
    let (record, cost) = record(&st.data, &plan)?;
    Ok(Split { data: st.data, record, counts: plan.counts()?, used: st.used, cost })
}

/// The plan that `record` codes over `data`, of the counts `c`.
pub fn plan(data: &[u8], record: &[u8], c: &Counts) -> Result<Plan> {
    let mut dec = ArithDecoder::new(record);
    let mut p = Plan::with_room(c);
    code(&mut dec, data, None, &mut p)?;
    code_fill(&mut dec, None, &mut p)?;
    if dec.overrun() {
        return Err(corrupt());
    }
    let got = (p.parts.len(), p.segs.len(), p.matches.len(), p.freqs.len(), p.zd.len());
    if got != (c.parts, c.segs, c.matches, c.freqs, c.zd) {
        return Err(corrupt());
    }
    Ok(p)
}

/// The stream that `data` and `record` rebuild, of the counts `c`, appended to `out`.
pub fn join_onto(data: &[u8], record: &[u8], c: &Counts, out: &mut Vec<u8>) -> Result<()> {
    plan::join(data, &plan(data, record, c)?, c, out)
}
