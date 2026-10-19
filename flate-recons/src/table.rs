//! The rules' tables of recent positions, specialized by shape (entries per bucket, key
//! length, second key length) so that the per-byte work is a handful of instructions.
//!
//! A table keeps, per hash of the first `K` bytes, the last `W` positions inserted (most
//! recent first), and with `L` a second such table keyed by 5 to 8 bytes. Every position is
//! inserted, or only token starts (and the inside of matches up to some length). Loads go
//! unchecked where at least 8 bytes are left; the last few positions take a checked path.

use crate::matchfind::{MAX_MATCH, WINDOW};

/// Bits of the hashes, for ranges of at least 2^15 positions (fewer for shorter ones, down to
/// [`MIN_BITS`]).
const HASH_BITS: u32 = 15;
const MIN_BITS: u32 = 10;
/// Entries per bucket of the second table.
const LW: usize = 1;
/// Entries of the chain ring (twice the window, so that a link is never overwritten while
/// its position is within the window of any position searched).
const CHAIN: usize = 2 * WINDOW;

/// What corrections need of a table: the candidates of a position, and the ranks of
/// distances beyond them (if the rule asks for them).
pub trait Cands {
    /// The entries of `p`'s buckets before `p`, most recent first (first table, then the
    /// second's not already listed).
    fn entries(&mut self, d: &[u8], p: usize) -> Vec<usize>;
    /// The index in [`Cands::entries`] of the entry at `p - dist`, if any.
    fn index_of(&mut self, d: &[u8], p: usize, dist: usize) -> Option<usize> {
        self.entries(d, p).iter().position(|&c| p - c == dist)
    }
    /// Whether distances are ranked.
    fn ranked(&self) -> bool {
        false
    }
    /// The rank of `dist` among the earlier positions matching `len` bytes at `p` that are
    /// not among the first table's entries (walking at most `limit` of them).
    fn rank(
        &mut self,
        _d: &[u8],
        _p: usize,
        _len: usize,
        _dist: usize,
        _limit: usize,
    ) -> Option<usize> {
        None
    }
    /// Inverse of [`Cands::rank`]: the distance of the given rank.
    fn select(
        &mut self,
        _d: &[u8],
        _p: usize,
        _len: usize,
        _rank: usize,
        _limit: usize,
    ) -> Option<usize> {
        None
    }
}

/// What predictions need of a table.
pub trait Search: Cands {
    /// The longest match at `p` among the candidates within `window`, the closest on ties:
    /// (length, distance), length 0 if none of at least 3.
    fn search(&mut self, d: &[u8], p: usize, window: usize) -> (usize, usize);
    /// [`Search::search`] among the candidates inserted so far, inserting nothing.
    fn peek(&mut self, d: &[u8], p: usize, window: usize) -> (usize, usize);
    /// Records that a token starts at `p` (token-driven insertion).
    fn token_start(&mut self, d: &[u8], p: usize);
    /// Records a token of `len` bytes at `p`.
    fn token(&mut self, d: &[u8], p: usize, len: usize);
}

/// No table (rules that do not search).
pub struct NoTable;

impl Cands for NoTable {
    fn entries(&mut self, _: &[u8], _: usize) -> Vec<usize> {
        vec![]
    }
}

impl Search for NoTable {
    fn search(&mut self, _: &[u8], _: usize, _: usize) -> (usize, usize) {
        (0, 0)
    }
    fn peek(&mut self, _: &[u8], _: usize, _: usize) -> (usize, usize) {
        (0, 0)
    }
    fn token_start(&mut self, _: &[u8], _: usize) {}
    fn token(&mut self, _: &[u8], _: usize, _: usize) {}
}

/// The table of [`super::tokens::Kind::Chain`]: exact chains of every position, which need
/// no insertion (they are built from the data as searches move forward).
pub struct ChainTable {
    m: crate::matchfind::Matcher,
    depth: usize,
    nice: usize,
}

/// Candidates walked when ranking a distance on exact chains.
const CHAIN_RANK_LIMIT: usize = 8192;

impl ChainTable {
    /// For searches at `start..end`, with the depth and nice length of the given indices.
    pub fn new(d: &[u8], start: usize, end: usize, depth: u8, nice: u8) -> Self {
        ChainTable {
            m: crate::matchfind::Matcher::single_range(d, start, end),
            depth: super::tokens::DEPTHS[depth as usize] as usize,
            nice: super::tokens::NICES[nice as usize] as usize,
        }
    }
}

impl Cands for ChainTable {
    fn entries(&mut self, _: &[u8], _: usize) -> Vec<usize> {
        vec![]
    }

    fn index_of(&mut self, _: &[u8], _: usize, _: usize) -> Option<usize> {
        None
    }

    fn ranked(&self) -> bool {
        true
    }

    fn rank(&mut self, d: &[u8], p: usize, len: usize, dist: usize, _: usize) -> Option<usize> {
        self.m.rank(d, p, len, dist, CHAIN_RANK_LIMIT)
    }

    fn select(&mut self, d: &[u8], p: usize, len: usize, rank: usize, _: usize) -> Option<usize> {
        self.m.select(d, p, len, rank, CHAIN_RANK_LIMIT)
    }
}

impl Search for ChainTable {
    fn search(&mut self, d: &[u8], p: usize, _: usize) -> (usize, usize) {
        let f = self.m.longest(d, p, self.depth, self.nice);
        if f.len < 3 { (0, 0) } else { (f.len, f.dist) }
    }

    fn peek(&mut self, d: &[u8], p: usize, window: usize) -> (usize, usize) {
        self.search(d, p, window)
    }

    fn token_start(&mut self, _: &[u8], _: usize) {}

    fn token(&mut self, _: &[u8], _: usize, _: usize) {}
}

pub struct Table<const W: usize, const K: usize, const L: bool> {
    bits: u32,
    /// Left shift keeping the second table's key bytes.
    lshift: u32,
    t: Vec<[u32; W]>,
    t2: Vec<[u32; LW]>,
    /// Token-driven insertion: starts, and the inside of matches up to this long.
    inner: Option<usize>,
    /// Every position before this is inserted (every-position policy).
    next: usize,
    last_start: usize,
    /// The last search: (position, window, result), until something is inserted.
    memo: (usize, usize, (usize, usize)),
    /// For ranking, a hash chain of every position by 3-byte keys: the previous entry of
    /// each position's chain, by position modulo its length ([`CHAIN`], or a power of two
    /// covering every position that can be inserted). When the first table has every
    /// position by 3 bytes, its buckets head the chains (`own` false); otherwise `heads`
    /// does, and positions are linked up to `linked` as walks need them.
    prev: Vec<u32>,
    own: bool,
    heads: Vec<u32>,
    linked: usize,
}

#[inline(always)]
fn load32(d: &[u8], p: usize) -> u32 {
    debug_assert!(p + 4 <= d.len());
    // SAFETY: callers keep `p + 8 <= d.len()` on the fast paths.
    unsafe { (d.as_ptr().add(p) as *const u32).read_unaligned() }
}

#[inline(always)]
fn load64(d: &[u8], p: usize) -> u64 {
    debug_assert!(p + 8 <= d.len());
    // SAFETY: as for `load32`.
    unsafe { (d.as_ptr().add(p) as *const u64).read_unaligned() }
}

/// Length of the common prefix of `d[a..]` and `d[p..]` (`a < p`), up to `max`
/// (`p + max <= d.len()`).
#[inline(always)]
pub fn common(d: &[u8], a: usize, p: usize, max: usize) -> usize {
    debug_assert!(a < p && p + max <= d.len());
    let mut l = 0;
    while l + 8 <= max {
        let x = load64(d, a + l) ^ load64(d, p + l);
        if x != 0 {
            return l + (x.trailing_zeros() / 8) as usize;
        }
        l += 8;
    }
    while l < max && d[a + l] == d[p + l] {
        l += 1;
    }
    l
}

#[inline(always)]
fn push<const W: usize>(b: &mut [u32; W], v: u32) {
    let mut n = [v; W];
    n[1..].copy_from_slice(&b[..W - 1]);
    *b = n;
}

impl<const W: usize, const K: usize, const L: bool> Table<W, K, L> {
    const MASK: u32 = if K == 3 { 0xff_ffff } else { u32::MAX };

    /// A table for coding tokens from `start` on, below `end`: with every position inserted,
    /// the window before `start` is inserted first. Short ranges get smaller tables (it
    /// matters for their setup, not for collisions).
    pub fn new(long: u8, inner: Option<usize>, start: usize, end: usize, rank: bool) -> Self {
        let span = end.saturating_sub(start.saturating_sub(WINDOW)).max(1);
        let bits = span
            .next_power_of_two()
            .trailing_zeros()
            .clamp(MIN_BITS, HASH_BITS);
        Table {
            bits,
            lshift: 64 - 8 * long as u32,
            t: vec![[0; W]; 1 << bits],
            t2: vec![[0; LW]; if L { 1 << bits } else { 0 }],
            inner,
            next: start.saturating_sub(WINDOW),
            last_start: usize::MAX,
            memo: (usize::MAX, 0, (0, 0)),
            // As many as positions can be inserted, if fewer.
            prev: if rank {
                vec![0; CHAIN.min((span + 1).next_power_of_two())]
            } else {
                vec![]
            },
            own: K != 3 || inner.is_some(),
            heads: if rank && (K != 3 || inner.is_some()) {
                vec![0; 1 << bits]
            } else {
                vec![]
            },
            linked: start.saturating_sub(WINDOW),
        }
    }

    #[inline(always)]
    fn hash(k: u32, bits: u32) -> usize {
        (k.wrapping_mul(0x9e37_79b1) >> (32 - bits)) as usize
    }

    /// The first table's bucket of `p` (`p + K <= d.len()`).
    #[inline(always)]
    fn h1(&self, d: &[u8], p: usize) -> usize {
        if p + 8 <= d.len() {
            Self::hash(load32(d, p) & Self::MASK, self.bits)
        } else {
            let k = d[p..p + K]
                .iter()
                .rev()
                .fold(0u32, |a, &b| a << 8 | b as u32);
            Self::hash(k, self.bits)
        }
    }

    /// The own chain head of `p` (`p + 3 <= d.len()`).
    #[inline(always)]
    fn h3(d: &[u8], p: usize, bits: u32) -> usize {
        Self::hash(
            d[p] as u32 | (d[p + 1] as u32) << 8 | (d[p + 2] as u32) << 16,
            bits,
        )
    }

    /// The second table's bucket of `p` (`p + 8 <= d.len()`).
    #[inline(always)]
    fn h2(d: &[u8], p: usize, bits: u32, lshift: u32) -> usize {
        let k = load64(d, p) << lshift;
        (k.wrapping_mul(0x9e37_79b9_7f4a_7c15) >> (64 - bits)) as usize
    }

    #[inline(always)]
    fn insert(&mut self, d: &[u8], p: usize) {
        self.memo.0 = usize::MAX;
        if p + 8 <= d.len() {
            self.insert_run(d, p, p + 1);
            return;
        }
        if p + K > d.len() {
            return;
        }
        let v = p as u32 + 1;
        let h = self.h1(d, p);
        let b = &mut self.t[h];
        if !self.prev.is_empty() && !self.own {
            let n = self.prev.len();
            self.prev[p & (n - 1)] = b[0];
        }
        push(b, v);
        if L && p + 8 <= d.len() {
            push(&mut self.t2[Self::h2(d, p, self.bits, self.lshift)], v);
        }
    }

    /// Walks the chain of `p` (past the first table's entries if it is that table's),
    /// closest first and within the window, calling `f` on each position until it returns
    /// false.
    fn walk(&mut self, d: &[u8], p: usize, limit: usize, mut f: impl FnMut(usize) -> bool) {
        self.ensure(d, p);
        if self.prev.is_empty() || p + K > d.len() {
            return;
        }
        let (mut e, skip) = if self.own {
            while self.linked < p {
                let q = self.linked;
                let h = &mut self.heads[Self::h3(d, q, self.bits)];
                let n = self.prev.len();
                self.prev[q & (n - 1)] = *h;
                *h = q as u32 + 1;
                self.linked += 1;
            }
            (self.heads[Self::h3(d, p, self.bits)] as usize, 0)
        } else {
            (self.t[self.h1(d, p)][0] as usize, W)
        };
        let (mut seen, mut steps) = (0, 0);
        while e > 0 && steps < limit {
            let c = e - 1;
            if c < p {
                if p - c > WINDOW {
                    break;
                }
                seen += 1;
                if seen > skip {
                    steps += 1;
                    if !f(c) {
                        break;
                    }
                }
            }
            e = self.prev[c & (self.prev.len() - 1)] as usize;
        }
    }

    /// Makes every position before `p` searchable (every-position policy).
    #[inline(always)]
    fn ensure(&mut self, d: &[u8], p: usize) {
        if self.inner.is_none() && self.next < p {
            self.memo.0 = usize::MAX;
            let fast = p.min(d.len().saturating_sub(7)).max(self.next);
            self.insert_run(d, self.next, fast);
            for q in fast..p {
                self.insert(d, q);
            }
            self.next = p;
        }
    }

    /// Inserts the positions `from..to`, which have 8 bytes after them.
    #[inline(always)]
    fn insert_run(&mut self, d: &[u8], from: usize, to: usize) {
        debug_assert!(to <= d.len().saturating_sub(7) || from >= to);
        let chain = !self.prev.is_empty() && !self.own;
        let (bits, lshift) = (self.bits, self.lshift);
        let (t, t2, prev) = (&mut self.t[..], &mut self.t2[..], &mut self.prev[..]);
        let ring = prev.len().wrapping_sub(1);
        for q in from..to {
            let v = q as u32 + 1;
            let b = &mut t[Self::hash(load32(d, q) & Self::MASK, bits)];
            if chain {
                prev[q & ring] = b[0];
            }
            push(b, v);
            if L {
                push(&mut t2[Self::h2(d, q, bits, lshift)], v);
            }
        }
    }

    /// The best of `bucket`'s candidates at `p` (where 8 bytes are left): the longest, the
    /// most recent on ties, given the best so far. Lengths are first measured on 8 bytes
    /// without branching; only candidates matching all 8 are measured further.
    #[inline(always)]
    fn best_of<const N: usize>(
        d: &[u8],
        p: usize,
        window: usize,
        max: usize,
        bucket: &[u32; N],
        best: (usize, usize),
    ) -> (usize, usize) {
        // Candidates as length << 32 | !distance: the greatest is the best.
        let key = |l: usize, dist: usize| (l as u64) << 32 | (!(dist as u32)) as u64;
        let x = load64(d, p);
        let mut top = if best.0 > 0 { key(best.0, best.1) } else { 0 };
        let mut long = 0u32;
        for (i, &e) in bucket.iter().enumerate() {
            let c = (e as usize).wrapping_sub(1);
            let ok = c < p && p - c <= window;
            // An invalid entry reads position 0 and is masked out.
            let cc = if ok { c } else { 0 };
            let l8 = ((load64(d, cc) ^ x).trailing_zeros() / 8) as usize;
            let l = if ok && l8 >= K { l8.min(max) } else { 0 };
            long |= ((l == 8) as u32) << i;
            let k = if l > 0 { key(l, p - cc) } else { 0 };
            top = top.max(k);
        }
        while long != 0 {
            let i = long.trailing_zeros() as usize;
            long &= long - 1;
            let c = bucket[i] as usize - 1;
            top = top.max(key(common(d, c, p, max), p - c));
        }
        if top == 0 {
            (0, 0)
        } else {
            ((top >> 32) as usize, !(top as u32) as usize)
        }
    }

    #[inline(always)]
    fn search_uncached(&self, d: &[u8], p: usize, window: usize, max: usize) -> (usize, usize) {
        if p + K > d.len() {
            return (0, 0);
        }
        if p + 8 <= d.len() {
            let mut best = Self::best_of(d, p, window, max, &self.t[self.h1(d, p)], (0, 0));
            if L {
                let b2 = &self.t2[Self::h2(d, p, self.bits, self.lshift)];
                best = Self::best_of(d, p, window, max, b2, best);
            }
            return if best.0 < 3 { (0, 0) } else { best };
        }
        let (mut best, mut dist) = (0, 0);
        let fast = p + 8 <= d.len();
        let kp = if fast { load32(d, p) & Self::MASK } else { 0 };
        let bucket = self.t[self.h1(d, p)];
        for e in bucket {
            if e == 0 {
                break;
            }
            let c = e as usize - 1;
            if c >= p {
                continue;
            }
            if p - c > window {
                break;
            }
            let same = if fast {
                load32(d, c) & Self::MASK == kp
            } else {
                d[c..c + K] == d[p..p + K]
            };
            if !same || (best > 0 && d[c + best] != d[p + best]) {
                continue;
            }
            let l = common(d, c, p, max);
            if l > best {
                (best, dist) = (l, p - c);
                if l == max {
                    return (best, dist);
                }
            }
        }
        if L && fast {
            for e in self.t2[Self::h2(d, p, self.bits, self.lshift)] {
                if e == 0 {
                    break;
                }
                let c = e as usize - 1;
                if c >= p || p - c > window {
                    continue;
                }
                if best > 0 && d[c + best] != d[p + best] {
                    continue;
                }
                let l = common(d, c, p, max);
                if l > best || (l == best && l > 0 && p - c < dist) {
                    (best, dist) = (l, p - c);
                    if l == max {
                        break;
                    }
                }
            }
        }
        if best < 3 { (0, 0) } else { (best, dist) }
    }
}

impl<const W: usize, const K: usize, const L: bool> Cands for Table<W, K, L> {
    fn entries(&mut self, d: &[u8], p: usize) -> Vec<usize> {
        self.ensure(d, p);
        if p + K > d.len() {
            return vec![];
        }
        let mut v: Vec<usize> = self.t[self.h1(d, p)]
            .iter()
            .map(|&e| e as usize)
            .filter(|&e| e > 0 && e - 1 < p)
            .map(|e| e - 1)
            .collect();
        if L && p + 8 <= d.len() {
            for e in self.t2[Self::h2(d, p, self.bits, self.lshift)] {
                let e = e as usize;
                if e > 0 && e - 1 < p && !v.contains(&(e - 1)) {
                    v.push(e - 1);
                }
            }
        }
        v
    }

    fn ranked(&self) -> bool {
        !self.prev.is_empty()
    }

    fn rank(&mut self, d: &[u8], p: usize, len: usize, dist: usize, limit: usize) -> Option<usize> {
        let (mut r, mut found) = (0, None);
        self.walk(d, p, limit, |c| {
            if p - c >= dist {
                // Chains go back in order: past the target, it is not there.
                found = (p - c == dist).then_some(r);
                return false;
            }
            r += (d[c + len - 1] == d[p + len - 1] && common(d, c, p, len) >= len) as usize;
            true
        });
        found
    }

    fn select(
        &mut self,
        d: &[u8],
        p: usize,
        len: usize,
        rank: usize,
        limit: usize,
    ) -> Option<usize> {
        let (mut r, mut found) = (0, None);
        self.walk(d, p, limit, |c| {
            if d[c + len - 1] == d[p + len - 1] && common(d, c, p, len) >= len {
                if r == rank {
                    found = Some(p - c);
                    return false;
                }
                r += 1;
            }
            true
        });
        found
    }

    fn index_of(&mut self, d: &[u8], p: usize, dist: usize) -> Option<usize> {
        if L || dist > p {
            return self.entries(d, p).iter().position(|&c| p - c == dist);
        }
        // One table: the entries before `p` in order, without collecting them.
        self.ensure(d, p);
        if p + K > d.len() {
            return None;
        }
        let target = (p - dist) as u32 + 1;
        self.t[self.h1(d, p)]
            .iter()
            .filter(|&&e| e > 0 && (e as usize - 1) < p)
            .position(|&e| e == target)
    }
}

impl<const W: usize, const K: usize, const L: bool> Search for Table<W, K, L> {
    #[inline(always)]
    fn search(&mut self, d: &[u8], p: usize, window: usize) -> (usize, usize) {
        let max = (d.len() - p).min(MAX_MATCH);
        if max < 3 {
            return (0, 0);
        }
        self.ensure(d, p);
        if (self.memo.0, self.memo.1) == (p, window) {
            return self.memo.2;
        }
        let r = self.search_uncached(d, p, window, max);
        self.memo = (p, window, r);
        r
    }

    #[inline(always)]
    fn peek(&mut self, d: &[u8], p: usize, window: usize) -> (usize, usize) {
        let max = (d.len() - p).min(MAX_MATCH);
        if max < 3 {
            return (0, 0);
        }
        self.search_uncached(d, p, window, max)
    }

    #[inline(always)]
    fn token_start(&mut self, d: &[u8], p: usize) {
        if self.inner.is_some() && self.last_start != p {
            self.insert(d, p);
            self.last_start = p;
        }
    }

    #[inline(always)]
    fn token(&mut self, d: &[u8], p: usize, len: usize) {
        let Some(inner) = self.inner else {
            return;
        };
        self.token_start(d, p);
        if len > 1 && len <= inner {
            self.memo.0 = usize::MAX;
            let fast = (p + len).min(d.len().saturating_sub(7)).max(p + 1);
            self.insert_run(d, p + 1, fast);
            for q in fast..p + len {
                self.insert(d, q);
            }
        }
    }
}
