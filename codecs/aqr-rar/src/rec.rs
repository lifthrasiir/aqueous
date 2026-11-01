//! What the records of RAR5's and RAR 3.x's streams share: their bits coded arithmetically
//! under adaptive contexts (`Bits`), and the match finder whose matches they are coded
//! against (`Mf`).

use aqr_common::arith::{ArithDecoder, ArithEncoder, Bit};

use crate::{Result, corrupt};

/// What the record codes, for the statistics.
#[derive(Clone, Copy, Debug)]
#[repr(usize)]
pub enum Cat {
    /// The files' sizes, the filters, the corrections.
    Head,
    /// Whether a match starts.
    Start,
    /// Which match: a repeated distance or a pair.
    Pair,
    /// A length shorter than the match's.
    Len,
    /// Matches not among the pairs.
    Escape,
    /// How a match is written (explicit, repeated, the last again).
    Form,
    /// The blocks: their sizes, tables.
    Blocks,
}
pub const CATS: usize = 7;

/// What codes the record's bits: the arithmetic encoder (which writes the value given) or
/// decoder (which returns what it reads).
pub trait Bits {
    fn bit(&mut self, b: &mut Bit, v: bool) -> bool;
    fn raw(&mut self, v: bool) -> bool;
    fn failed(&self) -> bool;
    /// What the bits after are, for the statistics.
    fn cat(&mut self, _: Cat) {}

    /// `v` as `x = v + 1`: the bit length of `x` less 1 in unary (adaptive bits of `t`), then
    /// the bits of `x` below its top one, raw.
    fn gamma(&mut self, t: &mut [Bit], v: u64) -> Result<u64> {
        let x = v.wrapping_add(1);
        let n = 63 - x.max(1).leading_zeros();
        let mut k = 0;
        while self.bit(&mut t[(k as usize).min(t.len() - 1)], k < n) {
            k += 1;
            if k > 63 || self.failed() {
                return Err(corrupt());
            }
        }
        let mut y = 1u64;
        for i in (0..k).rev() {
            y = y << 1 | self.raw(x >> i & 1 != 0) as u64;
        }
        Ok(y - 1)
    }

    /// `n` raw bits of `v`.
    fn raws(&mut self, n: u32, v: u64) -> u64 {
        (0..n).rev().fold(0, |x, i| x << 1 | self.raw(v >> i & 1 != 0) as u64)
    }

    /// `v` of `n` bits, by a binary tree of adaptive bits `t` (of `1 << n` bits).
    fn tree(&mut self, t: &mut [Bit], n: u32, v: u32) -> u32 {
        let mut k = 1usize;
        for i in (0..n).rev() {
            let b = self.bit(&mut t[k], v >> i & 1 != 0);
            k = k << 1 | b as usize;
        }
        (k - (1 << n)) as u32
    }
}

/// The encoder, counting the bits of each [`Cat`].
pub struct Enc {
    pub c: ArithEncoder,
    cat: Cat,
    pub cost: [f64; CATS],
}

impl Enc {
    pub fn new() -> Self {
        Enc { c: ArithEncoder::new(Vec::new()), cat: Cat::Head, cost: [0.0; CATS] }
    }
}

impl Default for Enc {
    fn default() -> Self {
        Self::new()
    }
}

impl Bits for Enc {
    fn bit(&mut self, b: &mut Bit, v: bool) -> bool {
        let p1 = b.p1();
        self.cost[self.cat as usize] -= ((if v { p1 } else { 65536 - p1 }) as f64 / 65536.0).log2();
        self.c.encode(v, p1);
        b.update(v);
        v
    }
    fn raw(&mut self, v: bool) -> bool {
        self.cost[self.cat as usize] += 1.0;
        self.c.encode(v, 1 << 15);
        v
    }
    fn failed(&self) -> bool {
        false
    }
    fn cat(&mut self, c: Cat) {
        self.cat = c;
    }
}

impl Bits for ArithDecoder<'_> {
    fn bit(&mut self, b: &mut Bit, _: bool) -> bool {
        let v = self.decode(b.p1());
        b.update(v);
        v
    }
    fn raw(&mut self, _: bool) -> bool {
        self.decode(1 << 15)
    }
    fn failed(&self) -> bool {
        self.overrun()
    }
}

/// `n` adaptive bits.
pub fn bits(n: usize) -> Vec<Bit> {
    vec![Bit::default(); n]
}

/// The length of the common prefix of `d[a..]` and `d[b..]`, `a < b`, at most `cap`.
#[inline]
pub fn lcp(d: &[u8], a: usize, b: usize, cap: usize) -> usize {
    let n = (d.len() - b).min(cap);
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

const NONE: u32 = u32::MAX;

/// The match finder: the positions before the current one, on a chain for each key length of
/// [`Mf::KEYS`], each chain holding the positions of its ring.
pub struct Mf<'a> {
    d: &'a [u8],
    reach: usize,
    head: Vec<Vec<u32>>,
    prev: Vec<Vec<u32>>,
    /// Positions below this are inserted.
    next: usize,
    found: Vec<(usize, usize)>,
}

/// Bits of the chains' heads.
const HEAD_BITS: u32 = 16;

/// The key of `k` bytes at `p` (`p + k` within the data).
#[inline]
fn key(d: &[u8], p: usize, k: usize) -> usize {
    let mut h = 0u64;
    for &b in &d[p..p + k] {
        h = (h ^ b as u64).wrapping_mul(0x100_0000_01b3);
    }
    (h.wrapping_mul(0x9e37_79b9_7f4a_7c15) >> (64 - HEAD_BITS)) as usize
}

impl<'a> Mf<'a> {
    /// Key lengths of the chains, and the most positions each holds (log 2): the chain of 2
    /// bytes serves near matches only.
    pub const KEYS: [(usize, u32); 5] = [(2, 10), (3, 18), (5, 22), (8, 22), (16, 22)];
    /// Candidates looked at on each chain for the longest matches.
    pub const DEPTH: usize = 32;
    /// Candidates looked at for an escape's rank.
    pub const LIMIT: usize = 4096;

    pub fn new(d: &'a [u8], reach: usize) -> Self {
        let span = d.len().max(2).next_power_of_two();
        Mf {
            d,
            reach,
            head: Self::KEYS.iter().map(|_| vec![NONE; 1 << HEAD_BITS]).collect(),
            prev: Self::KEYS.iter().map(|&(_, b)| vec![NONE; span.min(1 << b)]).collect(),
            next: 0,
            found: Vec::new(),
        }
    }

    /// Bytes of the match finder of `n` bytes of data.
    pub fn memory(n: u64) -> u64 {
        let span = n.max(2).next_power_of_two();
        Self::KEYS.iter().map(|&(_, b)| 4 * (span.min(1 << b) + (1 << HEAD_BITS))).sum()
    }

    /// Inserts the positions up to `pos`.
    pub fn advance(&mut self, pos: usize) {
        let d = self.d;
        while self.next < pos {
            let p = self.next;
            for (c, &(k, _)) in Self::KEYS.iter().enumerate() {
                if p + k <= d.len() {
                    let h = key(d, p, k);
                    let mask = self.prev[c].len() - 1;
                    self.prev[c][p & mask] = self.head[c][h];
                    self.head[c][h] = p as u32;
                }
            }
            self.next += 1;
        }
    }

    /// The farthest a chain `c` reaches back from `pos`.
    #[inline]
    fn far(&self, c: usize, pos: usize) -> usize {
        self.reach.min(self.prev[c].len() - 1).min(pos)
    }

    /// The longest matches at `pos` along the first [`Mf::DEPTH`] candidates of each chain,
    /// nearest first: each `(length, distance)` longer than those nearer, at most `cap` long,
    /// those that `ok` takes.
    pub fn pairs(&mut self, pos: usize, cap: usize, ok: impl Fn(usize, usize) -> bool, out: &mut Vec<(usize, usize)>) {
        out.clear();
        let d = self.d;
        let mut found = std::mem::take(&mut self.found);
        found.clear();
        let mut best = 1;
        for (c, &(k, _)) in Self::KEYS.iter().enumerate() {
            if pos + k > d.len() || k > cap {
                break;
            }
            let mask = self.prev[c].len() - 1;
            let far = self.far(c, pos);
            let mut q = self.head[c][key(d, pos, k)];
            let mut top = k - 1;
            for _ in 0..Self::DEPTH {
                if q == NONE || q as usize >= pos || pos - q as usize > far {
                    break;
                }
                let p = q as usize;
                if d[p + top] == d[pos + top] {
                    let l = lcp(d, p, pos, cap);
                    if l > top && ok(l, pos - p) {
                        top = l;
                        found.push((pos - p, l));
                        if l == cap {
                            break;
                        }
                    }
                }
                q = self.prev[c][p & mask];
            }
            best = best.max(top);
            if best == cap {
                break;
            }
        }
        // the nearest of each length
        found.sort_unstable();
        let mut best = 1;
        for &(dist, l) in found.iter() {
            if l > best {
                best = l;
                out.push((l, dist));
            }
        }
        self.found = found;
    }

    /// The chain an escape of `len` bytes is ranked on: the longest key within it.
    fn rank_chain(len: usize) -> usize {
        Self::KEYS.iter().rposition(|&(k, _)| k <= len.min(5)).unwrap_or(0)
    }

    /// The rank of the match `dist` back of at least `len` bytes at `pos` among those that
    /// `ok` takes on its chain, nearest first, among the first [`Mf::LIMIT`] candidates; where
    /// it is not among them, how many there are.
    pub fn rank(&self, pos: usize, len: usize, dist: usize, ok: impl Fn(usize) -> bool) -> usize {
        let mut count = 0;
        let mut found = false;
        self.walk(pos, len, |q| {
            if !ok(pos - q) {
                return true;
            }
            found = pos - q == dist;
            count += !found as usize;
            !found
        });
        count
    }

    /// The distance of the match of rank `r` (as [`Mf::rank`]).
    pub fn nth(&self, pos: usize, len: usize, r: usize, ok: impl Fn(usize) -> bool) -> Option<usize> {
        let (mut k, mut out) = (0, None);
        self.walk(pos, len, |q| {
            if !ok(pos - q) {
                return true;
            }
            if k == r {
                out = Some(pos - q);
            }
            k += 1;
            out.is_none()
        });
        out
    }

    /// Calls `f` with each position matching at least `len` bytes at `pos` on the chain of
    /// [`Mf::rank_chain`], nearest first, among its first [`Mf::LIMIT`] within reach, while it
    /// returns true.
    fn walk(&self, pos: usize, len: usize, mut f: impl FnMut(usize) -> bool) {
        let d = self.d;
        let c = Self::rank_chain(len);
        let k = Self::KEYS[c].0;
        if pos + k > d.len() || pos + len > d.len() {
            return;
        }
        let mask = self.prev[c].len() - 1;
        let far = self.far(c, pos);
        let mut q = self.head[c][key(d, pos, k)];
        for _ in 0..Self::LIMIT {
            if q == NONE || q as usize >= pos || pos - q as usize > far {
                return;
            }
            let p = q as usize;
            if d[p + len - 1] == d[pos + len - 1] && d[p..p + len] == d[pos..pos + len] && !f(p) {
                return;
            }
            q = self.prev[c][p & mask];
        }
    }
}
