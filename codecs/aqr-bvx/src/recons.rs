//! The record of a stream given its data (`FORMAT.md` §6): its segments and blocks, the parse
//! of each segment under contexts of what a match finder sees in the data, and what LZFSE
//! blocks hold beyond their usual choices.
//!
//! In LZFSE segments, a match is coded first as one of the two that the compressor's own table
//! offers there (it nearly always is); else, as LZVN's are, as which of the nearest matches of
//! increasing lengths it is, its distance not at all where it is the nearest for its length.

use aqr_common::arith::{ArithDecoder, ArithEncoder, Bit};

use crate::bvx::{self, D_MAX, Freqs};
use crate::plan::{self, LMD_LOW, Match, Part, Plan, Seg, Side};
use crate::{Error, Result, corrupt};

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
/// Bytes of the match finder's tables.
pub const MF_BYTES: u64 = ((RING + (1 << HEAD_BITS)) * 4) as u64;

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

/// The length of the common prefix of `d[a..]` and `d[b..]`, `a < b`, up to the end of `d`.
#[inline]
pub fn lcp(d: &[u8], a: usize, b: usize) -> usize {
    let n = d.len() - b;
    let mut i = 0;
    while i + 8 <= n {
        let x = u64::from_le_bytes(d[a + i..a + i + 8].try_into().unwrap())
            ^ u64::from_le_bytes(d[b + i..b + i + 8].try_into().unwrap());
        if x != 0 {
            return i + (x.trailing_zeros() / 8) as usize;
        }
        i += 8;
    }
    while i < n && d[a + i] == d[b + i] {
        i += 1;
    }
    i
}

/// The match finder: every position before the current one on a chain of the hash of its first
/// `min` bytes. Candidates are those whose first `min` bytes are the current position's.
struct Mf<'a> {
    d: &'a [u8],
    k: Kind,
    head: Vec<u32>,
    prev: Vec<u32>,
    /// Positions below this are inserted.
    next: usize,
}

impl<'a> Mf<'a> {
    fn new(d: &'a [u8], k: Kind) -> Self {
        Mf { d, k, head: vec![NONE; 1 << HEAD_BITS], prev: vec![NONE; RING], next: 0 }
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
    tab: Vec<u32>,
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

/// The longer, or the nearer of two as long.
#[inline]
fn better(a: Cand, b: Cand) -> Cand {
    if b.0 > a.0 || b.0 == a.0 && b.0 > 0 && b.1 < a.1 { b } else { a }
}

impl<'a> Table<'a> {
    fn new(d: &'a [u8]) -> Self {
        Table { d, tab: vec![NONE; (1 << TABLE_BITS) * WAYS], next: 0, looked: [(usize::MAX, [NONE; WAYS]); 4] }
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

/// [`lcp`] up to `cap`.
#[inline]
fn lcp_up(d: &[u8], a: usize, b: usize, cap: usize) -> usize {
    if cap >= d.len() - b { lcp(d, a, b) } else { lcp(&d[..b + cap], a, b) }
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
    start: Vec<Bit>,
    take: Vec<Bit>,
    longer: [Bit; 64],
    full: Vec<Bit>,
    short: Vec<[Bit; 64]>,
    far: Vec<Bit>,
    dist_bits: [Bit; 64],
    /// Whether the match is the table's first guess, or its second.
    best: Vec<Bit>,
    second: Vec<Bit>,
}

impl Walk {
    fn new() -> Self {
        let v = |n| vec![Bit::default(); n];
        Walk {
            start: v(8 * 12 * 3),
            take: v(4 * 9),
            longer: [Bit::default(); 64],
            full: v(12),
            short: vec![[Bit::default(); 64]; 12],
            far: v(12),
            dist_bits: [Bit::default(); 64],
            best: v(7 * 12),
            second: v(7 * 12),
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
    fn raw(&mut self, n: u32, v: u32, cat: Cat) -> u32 {
        let mut x = 0;
        for i in (0..n).rev() {
            x = x << 1 | self.bit(&mut Bit::default(), v >> i & 1 != 0, cat) as u32;
        }
        x
    }

    /// `v < 2^32 - 1` as Elias gamma of `v + 1`: its bit length less 1 in unary under the
    /// adaptive bits `t` (the last one shared beyond), then the bits below the top one, raw.
    fn gamma(&mut self, t: &mut [Bit], v: u32, cat: Cat) -> Result<u32> {
        let x = v.wrapping_add(1);
        let len = u32::BITS - x.leading_zeros();
        let mut b = 0;
        while self.bit(&mut t[b.min(t.len() - 1)], b + 1 < len as usize, cat) {
            b += 1;
            if b == 31 {
                return Err(corrupt());
            }
        }
        let low = self.raw(b as u32, x, cat);
        Ok((1 << b | low) - 1)
    }
}

struct Enc {
    c: ArithEncoder,
    cost: [f64; CATS],
}

impl Coder for Enc {
    #[inline]
    fn bit(&mut self, b: &mut Bit, v: bool, cat: Cat) -> bool {
        let p1 = b.p1();
        self.cost[cat as usize] -= ((if v { p1 } else { 65536 - p1 }) as f64 / 65536.0).log2();
        self.c.encode(v, p1);
        b.update(v);
        v
    }
}

struct Dec<'a> {
    c: ArithDecoder<'a>,
}

impl Coder for Dec<'_> {
    #[inline]
    fn bit(&mut self, b: &mut Bit, _: bool, _: Cat) -> bool {
        let v = self.c.decode(b.p1());
        b.update(v);
        v
    }
}

/// Codes the parse of the segment `d[start..]` (`seg` given, or filled in), the positions before
/// `start` being in reach.
fn walk(
    c: &mut impl Coder,
    w: &mut Walk,
    d: &[u8],
    start: usize,
    k: Kind,
    seg: &mut Seg,
    encoding: bool,
) -> Result<()> {
    let mut mf = Mf::new(d, k);
    mf.advance(start);
    let mut table = (k.min == 4).then(|| Table::new(d));
    let mut pairs = Vec::with_capacity(DEPTH + 1);
    let (mut pos, mut run) = (start, 0u32);
    let mut next = 0;
    // (a match for every `min` bytes at most, as the compressors' are: the module's memory)
    let mut decoded = if encoding { Vec::new() } else { Vec::with_capacity((d.len() - start) / k.min + 1) };
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
        let want = encoding.then(|| seg.matches.get(next)).flatten();
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
            if encoding {
                if got != want {
                    return Err(Error("match not coded"));
                }
            } else {
                decoded.push(got);
            }
            next += 1;
            mf.skip(pos, n);
            pos += n;
            run = 0;
            continue;
        }
        mf.pairs(pos, &mut pairs);
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
        if encoding {
            if got != want {
                return Err(Error("match not coded"));
            }
        } else {
            decoded.push(got);
        }
        next += 1;
        mf.skip(pos, n);
        pos += n;
        run = 0;
    }
    if encoding {
        if next != seg.matches.len() || run != seg.tail {
            return Err(Error("parse beyond the data"));
        }
    } else {
        seg.matches = decoded;
        seg.tail = run;
    }
    Ok(())
}

/// Codes `plan` over `data` (given, or filled in).
fn code(c: &mut impl Coder, data: &[u8], plan: &mut Plan, encoding: bool) -> Result<()> {
    let mut ctx = Ctx::new();
    let segs = if encoding { plan::segments(&plan.parts) } else { Vec::new() };
    let mut segs_in = std::mem::take(&mut plan.segs).into_iter().peekable();
    let parts_in = std::mem::take(&mut plan.parts);
    let mut pi = 0;
    let mut si = 0;
    let mut at = 0usize;
    // (no data: no block, or a stored block of nothing, as the compressor writes it)
    if data.is_empty() && c.bit(&mut ctx.rest, parts_in == [Part::Stored(0)], Cat::Layout) {
        plan.parts.push(Part::Stored(0));
        pi = 1;
    }
    while at < data.len() {
        // the kind: LZFSE, LZVN or stored; and the bytes of data, the rest or fewer
        let (kind, want_len, want_parts) = if encoding {
            let (range, lzvn) =
                if si < segs.len() && segs[si].0.start == pi { segs[si].clone() } else { (pi..pi + 1, false) };
            let kind = match parts_in[pi] {
                Part::Stored(_) => 2,
                _ if lzvn => 1,
                _ => 0,
            };
            let len = match &parts_in[pi] {
                Part::Stored(n) | Part::Lzvn(n) => *n as usize,
                Part::Lzfse(..) => plan::seg_len(segs_in.peek().ok_or_else(corrupt)?) as usize,
            };
            (kind, len, range)
        } else {
            (0, 0, 0..0)
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
        match k {
            2 => {
                if n > u32::MAX as usize {
                    return Err(corrupt());
                }
                plan.parts.push(Part::Stored(n as u32));
                pi += 1;
            }
            1 => {
                let mut seg = if encoding { segs_in.next().ok_or_else(corrupt)? } else { Seg::default() };
                walk(c, &mut ctx.walks[1], d, at, LZVN, &mut seg, encoding)?;
                plan.parts.push(Part::Lzvn(n as u32));
                plan.segs.push(seg);
                pi += 1;
                si += 1;
            }
            _ => {
                let mut seg = if encoding { segs_in.next().ok_or_else(corrupt)? } else { Seg::default() };
                walk(c, &mut ctx.walks[0], d, at, LZFSE, &mut seg, encoding)?;
                // the blocks: their triples, of the full count but for the last
                let total = plan::triples(&seg).count() as u32;
                let (mut done, mut i) = (0, want_parts.start);
                while done < total {
                    let want = if encoding {
                        match &parts_in.get(i) {
                            Some(Part::Lzfse(n, _)) => *n,
                            _ => return Err(corrupt()),
                        }
                    } else {
                        0
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
                    let mut side = match &parts_in.get(i) {
                        Some(Part::Lzfse(_, s)) if encoding => s.clone(),
                        _ => usual_side(),
                    };
                    code_side(c, &mut ctx, &mut side, encoding)?;
                    plan.parts.push(Part::Lzfse(n, side));
                    done += n;
                    i += 1;
                }
                if encoding && i != want_parts.end {
                    return Err(Error("blocks not coded"));
                }
                pi = want_parts.end;
                plan.segs.push(seg);
                si += 1;
            }
        }
        at += n;
    }
    if encoding && (pi != parts_in.len() || segs_in.next().is_some()) {
        return Err(Error("plan beyond the data"));
    }
    Ok(())
}

fn usual_side() -> Side {
    Side { freqs: None, lit_start: [0; 4], lmd_start: [0; 3], lit_low: 0, lmd_low: LMD_LOW, pad: vec![], zd: vec![] }
}

/// Codes what an LZFSE block holds beyond its usual choices, but for its pad and distances of no
/// match, whose counts are known only once all the blocks are ([`code_fill`]).
fn code_side(c: &mut impl Coder, ctx: &mut Ctx, side: &mut Side, encoding: bool) -> Result<()> {
    if !c.bit(&mut ctx.usual, side.usual(), Cat::Layout) {
        let has = c.bit(&mut ctx.freqs, side.freqs.is_some(), Cat::Layout);
        if has {
            let mut f =
                side.freqs.take().map_or_else(|| Freqs { l: [0; 20], m: [0; 20], d: [0; 64], lit: [0; 256] }, |f| *f);
            for v in f.l.iter_mut().chain(&mut f.m).chain(&mut f.d).chain(&mut f.lit) {
                let x = c.gamma(&mut ctx.value, *v as u32, Cat::Layout)?;
                *v = u16::try_from(x).map_err(|_| corrupt())?;
            }
            side.freqs = Some(Box::new(f));
        } else {
            side.freqs = None;
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
        *side = usual_side();
    }
    Ok(())
}

/// Codes the pad and the distances of no match of each LZFSE block of `plan`, once its
/// triples are known.
fn code_fill(c: &mut impl Coder, plan: &mut Plan, encoding: bool) -> Result<()> {
    let mut ctx_pad = Bit::default();
    let mut ctx_zd = Bit::default();
    let mut zd_bits = [Bit::default(); 64];
    let shape = plan::shapes(plan)?;
    for (p, (pad, zd)) in plan.parts.iter_mut().filter(|p| matches!(p, Part::Lzfse(..))).zip(shape) {
        let Part::Lzfse(_, side) = p else { unreachable!() };
        if encoding && (side.pad.len() != pad || side.zd.len() != zd) {
            return Err(Error("side of another shape"));
        }
        side.pad.resize(pad, 0);
        side.zd.resize(zd, 0);
        for b in side.pad.iter_mut() {
            if !c.bit(&mut ctx_pad, *b == 0, Cat::Layout) {
                *b = c.raw(8, *b as u32, Cat::Layout) as u8;
            } else {
                *b = 0;
            }
        }
        for d in side.zd.iter_mut() {
            if !c.bit(&mut ctx_zd, *d == 0, Cat::Layout) {
                *d = 1 + c.gamma(&mut zd_bits, d.wrapping_sub(1), Cat::Layout)?;
            } else {
                *d = 0;
            }
        }
    }
    Ok(())
}

/// A stream split.
pub struct Split {
    pub data: Vec<u8>,
    pub record: Vec<u8>,
    /// Bytes of the stream, up to its end.
    pub used: usize,
    /// Bits of the record by [`Cat`].
    pub cost: [f64; CATS],
}

/// The record of `plan` over `data`.
pub fn record(data: &[u8], plan: &Plan) -> Result<(Vec<u8>, [f64; CATS])> {
    let mut e = Enc { c: ArithEncoder::new(Vec::new()), cost: [0.0; CATS] };
    let mut p = plan.clone();
    code(&mut e, data, &mut p, true)?;
    code_fill(&mut e, &mut p, true)?;
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
    Ok(Split { data: st.data, record, used: st.used, cost })
}

/// The plan that `record` codes over `data`.
pub fn plan(data: &[u8], record: &[u8]) -> Result<Plan> {
    let mut dec = Dec { c: ArithDecoder::new(record) };
    let mut p = Plan::default();
    code(&mut dec, data, &mut p, false)?;
    code_fill(&mut dec, &mut p, false)?;
    if dec.c.overrun() {
        return Err(corrupt());
    }
    Ok(p)
}

/// The stream that `data` and `record` rebuild, appended to `out`.
pub fn join_onto(data: &[u8], record: &[u8], out: &mut Vec<u8>) -> Result<()> {
    plan::join(data, &plan(data, record)?, out)
}
