//! The record of an LZO1X stream's parse given its data (`FORMAT.md` §4).
//!
//! Whether a match starts at a position is coded under contexts of the matches the data offers
//! there; a match's length against the longest ones a hash chain finds, and its distance not at
//! all where it is the nearest one for its length. Both sides see the same data, so the
//! contexts are the same.

use aqr_common::arith::{ArithDecoder, ArithEncoder, Bit};

use crate::lzo1x::{self, Form, Match, Parse, WINDOW, long_fits, short_fits, state};
use crate::{Error, Result, corrupt};

/// Candidates looked at for the longest matches, on the chain of 3 bytes.
pub const DEPTH: usize = 32;
/// Candidates looked at for the nearest match of a length beyond those.
pub const LIMIT: usize = 1024;

/// The record's first byte: the positions inside matches are left out of the match finder (as
/// the hash table of a fast compressor leaves them out).
pub const SKIP: u8 = 1;

const NONE: u32 = u32::MAX;
const RING: usize = 1 << 16;

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

/// Bucket of a distance: below 64, 1024, 8192, or beyond.
#[inline]
fn db(d: usize) -> usize {
    (d >= 64) as usize + (d >= 1024) as usize + (d >= 8192) as usize
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

/// The match finder: the positions before the current one, on a chain of their first 2 bytes
/// and one of a hash of their first 3. Rings of 2^16 suffice, as matches reach 49151 back.
struct Mf<'a> {
    d: &'a [u8],
    head2: Vec<u32>,
    prev2: Vec<u32>,
    head3: Vec<u32>,
    prev3: Vec<u32>,
    /// Positions below this are inserted.
    next: usize,
}

#[inline]
fn k2(d: &[u8], p: usize) -> usize {
    d[p] as usize | (d[p + 1] as usize) << 8
}

#[inline]
fn h3(d: &[u8], p: usize) -> usize {
    let k = d[p] as u32 | (d[p + 1] as u32) << 8 | (d[p + 2] as u32) << 16;
    (k.wrapping_mul(0x9e37_79b1) >> 16) as usize
}

impl<'a> Mf<'a> {
    fn new(d: &'a [u8]) -> Self {
        let v = || vec![NONE; RING];
        Mf { d, head2: v(), prev2: v(), head3: v(), prev3: v(), next: 0 }
    }

    /// Inserts the positions up to `pos`.
    #[inline]
    fn advance(&mut self, pos: usize) {
        let d = self.d;
        while self.next < pos {
            let p = self.next;
            if p + 3 <= d.len() {
                let h = h3(d, p);
                self.prev3[p % RING] = self.head3[h];
                self.head3[h] = p as u32;
            }
            if p + 2 <= d.len() {
                let k = k2(d, p);
                self.prev2[p % RING] = self.head2[k];
                self.head2[k] = p as u32;
            }
            self.next += 1;
        }
    }

    /// `c` if it is a position within reach of `pos`.
    #[inline]
    fn reach(c: u32, pos: usize) -> Option<usize> {
        (c != NONE && pos - c as usize <= WINDOW).then_some(c as usize)
    }

    /// The nearest position of the first 2 bytes of `pos`.
    #[inline]
    fn near2(&self, pos: usize) -> Option<usize> {
        if pos + 2 > self.d.len() {
            return None;
        }
        Self::reach(self.head2[k2(self.d, pos)], pos)
    }

    /// The nearest position of the hash of the first 3 bytes of `pos`.
    #[inline]
    fn near3(&self, pos: usize) -> Option<usize> {
        if pos + 3 > self.d.len() {
            return None;
        }
        Self::reach(self.head3[h3(self.d, pos)], pos)
    }

    /// The longest matches at `pos`, nearest first, from the nearest of 2 bytes `c2` and the
    /// first [`DEPTH`] candidates of the chain of 3: each longer than those before it.
    fn pairs(&self, pos: usize, c2: usize, out: &mut Vec<(usize, usize)>) {
        let d = self.d;
        let cap = d.len() - pos;
        out.clear();
        let mut t = lcp(d, c2, pos);
        out.push((t, c2));
        let mut c = self.near3(pos);
        for _ in 0..DEPTH {
            let Some(p) = c.filter(|_| t < cap) else { break };
            if p < c2 && d[p + t] == d[pos + t] {
                let n = lcp(d, p, pos);
                if n > t {
                    t = n;
                    out.push((n, p));
                }
            }
            c = Self::reach(self.prev3[p % RING], pos);
        }
    }

    /// The nearest match of at least `n` bytes at `pos` (`2 ≤ n ≤` what is left), among the
    /// first [`LIMIT`] candidates of the chain of 2 bytes (n = 2) or 3.
    fn nearest(&self, pos: usize, n: usize) -> Option<usize> {
        let d = self.d;
        let (mut c, prev) = if n == 2 { (self.near2(pos), &self.prev2) } else { (self.near3(pos), &self.prev3) };
        for _ in 0..LIMIT {
            let p = c?;
            if d[p + n - 1] == d[pos + n - 1] && lcp(d, p, pos) >= n {
                return Some(p);
            }
            c = Self::reach(prev[p % RING], pos);
        }
        None
    }
}

/// What the record codes, for the statistics.
#[derive(Clone, Copy)]
#[repr(usize)]
pub enum Cat {
    /// Whether a match starts, and the first run's form.
    Start,
    Len,
    Dist,
    Form,
}
pub const CATS: usize = 4;

struct Ctx {
    start: Vec<Bit>,
    first_short: Bit,
    /// A match: which pair (from the longest), whether at its length, else how much shorter.
    pair: Vec<Bit>,
    full: Vec<Bit>,
    short: Vec<[Bit; 64]>,
    longer: [Bit; 64],
    far: Vec<Bit>,
    dist_bits: [Bit; 64],
    form: Vec<Bit>,
}

impl Ctx {
    fn new() -> Self {
        let v = |n| vec![Bit::default(); n];
        Ctx {
            start: v(8 * 12 * 12 * 4),
            first_short: Bit::default(),
            pair: v(8 * 9),
            full: v(4 * 12),
            short: vec![[Bit::default(); 64]; 12],
            longer: [Bit::default(); 64],
            far: v(5 * 12),
            dist_bits: [Bit::default(); 64],
            form: v(2 * 8),
        }
    }
}

/// The side of the record's coder.
trait Side {
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

impl Side for Enc {
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

impl Side for Dec<'_> {
    #[inline]
    fn bit(&mut self, b: &mut Bit, _: bool, _: Cat) -> bool {
        let v = self.c.decode(b.p1());
        b.update(v);
        v
    }
}

/// Walks the parse over `d`, coding it (`parse` given) or decoding it (`parse` filled in),
/// as the side is.
fn walk(side: &mut impl Side, d: &[u8], parse: &mut Parse, encoding: bool, skip: bool) -> Result<()> {
    let mut mf = Mf::new(d);
    let mut ctx = Ctx::new();
    let mut pairs = Vec::with_capacity(DEPTH + 1);
    let (mut pos, mut run) = (0, 0u32);
    let mut next = 0;
    let mut first = true;
    while pos < d.len() {
        mf.advance(pos);
        let Some(c2) = mf.near2(pos) else {
            pos += 1;
            run += 1;
            continue;
        };
        let want = encoding.then(|| parse.matches.get(next)).flatten();
        let starts = want.is_some_and(|m| m.lits == run);
        let l2 = lcp(d, c2, pos);
        let (l3, far3) = mf.near3(pos).map_or((0, 0), |c| (lcp(d, c, pos), db(pos - c)));
        let b = &mut ctx.start[((bucket(run) * 12 + lb(l2)) * 12 + lb(l3)) * 4 + far3];
        if !side.bit(b, starts, Cat::Start) {
            pos += 1;
            run += 1;
            continue;
        }
        if first {
            first = false;
            let v = parse.first_short;
            parse.first_short = first_run(side, &mut ctx.first_short, run, v)?;
        }
        let want = want.copied().unwrap_or(Match { lits: run, len: 0, dist: 0, form: Form::Short });
        mf.pairs(pos, c2, &mut pairs);
        let m = pairs.len();
        let (len, dist) = (want.len as usize, want.dist as usize);
        // Which pair: the first from the longest whose length is at least the match's.
        let j_want = if encoding {
            pairs.iter().rev().take_while(|&&(l, _)| l >= len).count().checked_sub(1).map_or(m, |j| j)
        } else {
            0
        };
        let mut j = 0;
        while j < m && side.bit(&mut ctx.pair[j.min(7) * 9 + m.min(8)], j_want > j, Cat::Len) {
            j += 1;
        }
        let (n, near) = if j == m {
            let top = pairs[m - 1].0;
            let v = (len.wrapping_sub(top + 1)) as u32;
            let n = top + 1 + side.gamma(&mut ctx.longer, v, Cat::Len)? as usize;
            if n > d.len() - pos {
                return Err(corrupt());
            }
            (n, mf.nearest(pos, n).map(|c| pos - c))
        } else {
            let i = m - 1 - j;
            let (top, c) = pairs[i];
            let lo = if i > 0 { pairs[i - 1].0 + 1 } else { 2 };
            let n = if side.bit(&mut ctx.full[j.min(3) * 12 + lb(top)], len == top, Cat::Len) {
                top
            } else {
                let v = (top.wrapping_sub(1).wrapping_sub(len)) as u32;
                let k = side.gamma(&mut ctx.short[lb(top)], v, Cat::Len)? as usize;
                top.checked_sub(1 + k).filter(|&n| n >= lo).ok_or_else(corrupt)?
            };
            (n, Some(pos - c))
        };
        let n32 = n as u32;
        let far = match near {
            Some(nd) => side.bit(&mut ctx.far[state(run) as usize * 12 + lb(n)], dist != nd, Cat::Dist),
            None => true,
        };
        let dist = if far {
            let b = usize::BITS - dist.leading_zeros();
            let mut m = 1;
            for i in (0..5).rev() {
                m = m << 1 | side.bit(&mut ctx.dist_bits[m], b >> i & 1 != 0, Cat::Dist) as usize;
            }
            let b = m as u32 - 32;
            if !(1..=16).contains(&b) {
                return Err(corrupt());
            }
            (1 << (b - 1) | side.raw(b - 1, dist as u32, Cat::Dist)) as usize
        } else {
            near.unwrap()
        };
        if dist > pos.min(WINDOW) || n > d.len() - pos || lcp(d, pos - dist, pos) < n {
            return Err(corrupt());
        }
        let (s, l) = (short_fits(run, n32, dist as u32), long_fits(n32, dist as u32));
        let form = match (s, l) {
            (true, true) => {
                let k = (state(run) == 4 && n == 3) as usize * 8 + n.min(9) - 2;
                if side.bit(&mut ctx.form[k], want.form == Form::Long, Cat::Form) { Form::Long } else { Form::Short }
            }
            (true, false) => Form::Short,
            (false, true) => Form::Long,
            (false, false) => return Err(corrupt()),
        };
        let got = Match { lits: run, len: n32, dist: dist as u32, form };
        if encoding {
            if got != want {
                return Err(Error("match not coded"));
            }
        } else {
            parse.matches.push(got);
        }
        mf.advance(pos + 1);
        if skip {
            mf.next = pos + n;
        }
        next += 1;
        pos += n;
        run = 0;
    }
    if first {
        let v = parse.first_short;
        parse.first_short = first_run(side, &mut ctx.first_short, run, v)?;
    }
    if encoding && next != parse.matches.len() {
        return Err(Error("parse beyond the data"));
    }
    parse.tail = run;
    Ok(())
}

/// The first run's form, once its length is known: coded where both forms are possible.
fn first_run(side: &mut impl Side, b: &mut Bit, run: u32, v: bool) -> Result<bool> {
    Ok(match run {
        0 => false,
        1..=3 => true,
        4..=238 => side.bit(b, v, Cat::Start),
        _ => false,
    })
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

/// The record of the parse `parse` over `data`, with the match finder of `mode` ([`SKIP`]).
pub fn record(data: &[u8], parse: &Parse, mode: u8) -> Result<(Vec<u8>, [f64; CATS])> {
    let mut e = Enc { c: ArithEncoder::new(vec![mode]), cost: [0.0; CATS] };
    let mut p = parse.clone();
    walk(&mut e, data, &mut p, true, mode & SKIP != 0)?;
    if p != *parse {
        return Err(Error("parse not coded"));
    }
    Ok((e.c.finish(), e.cost))
}

/// Data of a stream coded under both modes to pick one.
const TRIAL: usize = 1 << 16;

/// The mode whose record of the first [`TRIAL`] bytes or so of `data` is the smaller.
fn mode(data: &[u8], parse: &Parse) -> u8 {
    let (mut at, mut k) = (0, 0);
    while k < parse.matches.len() && at < TRIAL {
        at += (parse.matches[k].lits + parse.matches[k].len) as usize;
        k += 1;
    }
    let p = Parse { matches: parse.matches[..k].to_vec(), tail: 0, first_short: parse.first_short };
    let size = |mode| record(&data[..at], &p, mode).map_or(usize::MAX, |r| r.0.len());
    if size(0) <= size(SKIP) { 0 } else { SKIP }
}

/// Splits the LZO1X stream at the start of `s` into its data (at most `limit` bytes) and record.
pub fn split(s: &[u8], limit: usize) -> Result<Split> {
    let dec = lzo1x::decode(s, limit)?;
    if lzo1x::encode(&dec.data, &dec.parse) != s[..dec.used] {
        return Err(Error("stream not given back"));
    }
    let (record, cost) =
        record(&dec.data, &dec.parse, mode(&dec.data, &dec.parse)).or_else(|_| record(&dec.data, &dec.parse, SKIP))?;
    Ok(Split { data: dec.data, record, used: dec.used, cost })
}

/// The parse that `record` codes over `data`.
pub fn parse(data: &[u8], record: &[u8]) -> Result<Parse> {
    let (&mode, record) = record.split_first().ok_or_else(corrupt)?;
    if mode & !SKIP != 0 {
        return Err(corrupt());
    }
    let mut dec = Dec { c: ArithDecoder::new(record) };
    let mut p = Parse::default();
    walk(&mut dec, data, &mut p, false, mode & SKIP != 0)?;
    if dec.c.overrun() {
        return Err(corrupt());
    }
    Ok(p)
}

/// The stream that `data` and `record` rebuild.
pub fn join(data: &[u8], record: &[u8]) -> Result<Vec<u8>> {
    Ok(lzo1x::encode(data, &parse(data, record)?))
}
