//! The rules' tables of recent positions, specialized by shape (entries per bucket, key
//! length, second key length) so that the per-byte work is a handful of instructions.
//!
//! A table keeps, per hash of the first `K` bytes, the last `W` positions inserted (most
//! recent first), and with `L` a second such table keyed by 5 to 8 bytes. Every position is
//! inserted, or only token starts (and the inside of matches up to some length). Loads go
//! unchecked where at least 8 bytes are left; the last few positions take a checked path.

use crate::matchfind::{Found, MAX_MATCH, Matcher, WINDOW};

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
    m: Matcher,
    depth: usize,
    nice: usize,
}

/// Candidates walked when ranking a distance on exact chains.
const CHAIN_RANK_LIMIT: usize = 8192;

impl ChainTable {
    /// For searches at `start..end`, with the depth and nice length of the given indices.
    pub fn new(d: &[u8], start: usize, end: usize, depth: u8, nice: u8) -> Self {
        ChainTable {
            m: Matcher::single_range(d, start, end),
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

/// Configurations a [`MultiChain`] searches at once, at most.
pub const MULTI: usize = 4;

/// [`ChainTable`] for several (depth, nice) configurations at once: one walk of a chain
/// gives every configuration's match ([`Matcher::longest_multi`]).
pub struct MultiChain {
    m: Matcher,
    cfgs: Vec<(usize, usize)>,
    /// The last two searches: position and every configuration's match, the last one at
    /// `memo[last]`.
    memo: [(usize, [Found; MULTI]); 2],
    last: usize,
    /// The last rank: (position, length, distance, rank).
    rank: (usize, usize, usize, Option<usize>),
}

impl MultiChain {
    /// With the (depth, nice) indices of `cfgs`, searching with `m` (a
    /// [`Matcher::single_range`] for the searches).
    pub fn new(m: Matcher, cfgs: &[(u8, u8)]) -> Self {
        assert!(cfgs.len() <= MULTI);
        MultiChain {
            m,
            cfgs: cfgs
                .iter()
                .map(|&(depth, nice)| {
                    (
                        super::tokens::DEPTHS[depth as usize] as usize,
                        super::tokens::NICES[nice as usize] as usize,
                    )
                })
                .collect(),
            memo: [(usize::MAX, [Found::default(); MULTI]); 2],
            last: 0,
            rank: (usize::MAX, 0, 0, None),
        }
    }

    /// Every configuration's longest match at `p` (the first [`MultiChain::new`] gave).
    pub fn search(&mut self, d: &[u8], p: usize) -> [Found; MULTI] {
        if let Some(m) = self.memo.iter().find(|m| m.0 == p) {
            return m.1;
        }
        self.last ^= 1;
        let (at, out) = &mut self.memo[self.last];
        *at = p;
        let n = self.cfgs.len();
        self.m.longest_multi(d, p, &self.cfgs, &mut out[..n]);
        *out
    }
}

impl Cands for MultiChain {
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
        // Configurations that miss the same token rank the same distance.
        if (self.rank.0, self.rank.1, self.rank.2) != (p, len, dist) {
            let r = self.m.rank(d, p, len, dist, CHAIN_RANK_LIMIT);
            self.rank = (p, len, dist, r);
        }
        self.rank.3
    }

    fn select(&mut self, d: &[u8], p: usize, len: usize, rank: usize, _: usize) -> Option<usize> {
        self.m.select(d, p, len, rank, CHAIN_RANK_LIMIT)
    }
}

/// A table with `W` entries per bucket of the first table, keyed by `K` bytes (3 or 4), with a
/// second table if `L`: the searches, insertions and ranking walks, the work per byte or per
/// miss, are specialized by its shape (each parameter matters for speed); the rest is
/// [`Core`]'s, the same for all.
pub struct Table<const W: usize, const K: usize, const L: bool>(Core);

/// The state of a [`Table`], whatever its shape.
struct Core {
    bits: u32,
    /// Left shift keeping the second table's key bytes.
    lshift: u32,
    /// Bytes of the first table's key (3 or 4).
    key: usize,
    /// Whether there is a second table.
    long: bool,
    /// [`Core::ensure`] for the table's shape (for [`Core`]'s own methods).
    ensure_fn: fn(&mut Core, &[u8], usize),
    /// The first table: `ways` entries per bucket, bucket after bucket.
    ways: usize,
    t: Vec<u32>,
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

/// The mask of a first table's key of `key` bytes, on 4 bytes.
const fn mask_of(key: usize) -> u32 {
    if key == 3 { 0xff_ffff } else { u32::MAX }
}

#[inline(always)]
const fn mask<const K: usize>() -> u32 {
    mask_of(K)
}

#[inline(always)]
fn push<const W: usize>(b: &mut [u32; W], v: u32) {
    let mut n = [v; W];
    n[1..].copy_from_slice(&b[..W - 1]);
    *b = n;
}

impl<const W: usize, const K: usize, const L: bool> Table<W, K, L> {
    /// A table for coding tokens from `start` on, below `end`, with a second table keyed by
    /// `long` bytes (if `L`): with every position inserted, the window before `start` is
    /// inserted first. Short ranges get smaller tables (it matters for their setup, not for
    /// collisions).
    pub fn new(long: u8, inner: Option<usize>, start: usize, end: usize, rank: bool) -> Self {
        debug_assert!(L == (long > 0));
        let ensure = |c: &mut Core, d: &[u8], p: usize| c.ensure::<W, K, L>(d, p);
        Table(Core::new(W, K, long, ensure, inner, start, end, rank))
    }
}

impl Core {
    fn new(
        ways: usize,
        key: usize,
        long: u8,
        ensure_fn: fn(&mut Core, &[u8], usize),
        inner: Option<usize>,
        start: usize,
        end: usize,
        rank: bool,
    ) -> Self {
        let span = end.saturating_sub(start.saturating_sub(WINDOW)).max(1);
        let bits = span
            .next_power_of_two()
            .trailing_zeros()
            .clamp(MIN_BITS, HASH_BITS);
        let own = key != 3 || inner.is_some();
        Core {
            bits,
            lshift: 64 - 8 * long as u32,
            key,
            long: long > 0,
            ensure_fn,
            ways,
            t: vec![0; ways << bits],
            t2: vec![[0; LW]; if long > 0 { 1 << bits } else { 0 }],
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
            own,
            heads: if rank && own {
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

    /// The first table's bucket of `p` (`p + key <= d.len()`).
    #[inline(always)]
    fn h1(&self, d: &[u8], p: usize) -> usize {
        if p + 8 <= d.len() {
            Self::hash(load32(d, p) & mask_of(self.key), self.bits)
        } else {
            let k = d[p..p + self.key]
                .iter()
                .rev()
                .fold(0u32, |a, &b| a << 8 | b as u32);
            Self::hash(k, self.bits)
        }
    }

    /// [`Core::h1`] on the per-byte paths.
    #[inline(always)]
    fn h1k<const K: usize>(&self, d: &[u8], p: usize) -> usize {
        if p + 8 <= d.len() {
            Self::hash(load32(d, p) & mask::<K>(), self.bits)
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

    /// Bucket `h` of the first table, of `W` (its `ways`) entries.
    #[inline(always)]
    fn bucket<const W: usize>(t: &[u32], h: usize) -> &[u32; W] {
        &t.as_chunks::<W>().0[h]
    }

    #[inline(always)]
    fn bucket_mut<const W: usize>(t: &mut [u32], h: usize) -> &mut [u32; W] {
        &mut t.as_chunks_mut::<W>().0[h]
    }

    /// Bucket `h` of the first table, whatever its `ways`.
    fn bucket_any(&self, h: usize) -> &[u32] {
        &self.t[h * self.ways..(h + 1) * self.ways]
    }

    #[inline(always)]
    fn insert<const W: usize, const K: usize, const L: bool>(&mut self, d: &[u8], p: usize) {
        self.memo.0 = usize::MAX;
        if p + 8 <= d.len() {
            self.insert_run::<W, K, L>(d, p, p + 1);
            return;
        }
        if p + K > d.len() {
            return;
        }
        let v = p as u32 + 1;
        let h = self.h1k::<K>(d, p);
        let b = Self::bucket_mut::<W>(&mut self.t, h);
        if !self.prev.is_empty() && !self.own {
            let n = self.prev.len();
            self.prev[p & (n - 1)] = b[0];
        }
        push(b, v);
    }

    /// Walks the chain of `p` (past the first table's entries if it is that table's),
    /// closest first and within the window, calling `f` on each position until it returns
    /// false.
    #[inline(always)]
    fn walk<const W: usize, const K: usize, const L: bool>(
        &mut self,
        d: &[u8],
        p: usize,
        limit: usize,
        mut f: impl FnMut(usize) -> bool,
    ) {
        self.ensure::<W, K, L>(d, p);
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
            (
                Self::bucket::<W>(&self.t, self.h1k::<K>(d, p))[0] as usize,
                W,
            )
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
    fn ensure<const W: usize, const K: usize, const L: bool>(&mut self, d: &[u8], p: usize) {
        if self.inner.is_none() && self.next < p {
            self.memo.0 = usize::MAX;
            let fast = p.min(d.len().saturating_sub(7)).max(self.next);
            self.insert_run::<W, K, L>(d, self.next, fast);
            for q in fast..p {
                self.insert::<W, K, L>(d, q);
            }
            self.next = p;
        }
    }

    /// [`Core::ensure`] off the per-byte paths.
    fn ensure_any(&mut self, d: &[u8], p: usize) {
        (self.ensure_fn)(self, d, p)
    }

    /// Inserts the positions `from..to`, which have 8 bytes after them.
    #[inline(always)]
    fn insert_run<const W: usize, const K: usize, const L: bool>(
        &mut self,
        d: &[u8],
        from: usize,
        to: usize,
    ) {
        debug_assert!(to <= d.len().saturating_sub(7) || from >= to);
        let chain = !self.prev.is_empty() && !self.own;
        let (bits, lshift) = (self.bits, self.lshift);
        let (t, t2, prev) = (&mut self.t[..], &mut self.t2[..], &mut self.prev[..]);
        let ring = prev.len().wrapping_sub(1);
        for q in from..to {
            let v = q as u32 + 1;
            let b = Self::bucket_mut::<W>(t, Self::hash(load32(d, q) & mask::<K>(), bits));
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
    /// most recent on ties, given the best so far, of at least `key` bytes. Lengths are first
    /// measured on 8 bytes without branching; only candidates matching all 8 are measured
    /// further.
    #[inline(always)]
    fn best_of<const N: usize>(
        d: &[u8],
        p: usize,
        window: usize,
        max: usize,
        key: usize,
        bucket: &[u32; N],
        best: (usize, usize),
    ) -> (usize, usize) {
        // Candidates as length << 32 | !distance: the greatest is the best.
        let order = |l: usize, dist: usize| (l as u64) << 32 | (!(dist as u32)) as u64;
        let x = load64(d, p);
        let mut top = if best.0 > 0 { order(best.0, best.1) } else { 0 };
        let mut long = 0u32;
        for (i, &e) in bucket.iter().enumerate() {
            let c = (e as usize).wrapping_sub(1);
            let ok = c < p && p - c <= window;
            // An invalid entry reads position 0 and is masked out.
            let cc = if ok { c } else { 0 };
            let l8 = ((load64(d, cc) ^ x).trailing_zeros() / 8) as usize;
            let l = if ok && l8 >= key { l8.min(max) } else { 0 };
            long |= ((l == 8) as u32) << i;
            let k = if l > 0 { order(l, p - cc) } else { 0 };
            top = top.max(k);
        }
        while long != 0 {
            let i = long.trailing_zeros() as usize;
            long &= long - 1;
            let c = bucket[i] as usize - 1;
            top = top.max(order(common(d, c, p, max), p - c));
        }
        if top == 0 {
            (0, 0)
        } else {
            ((top >> 32) as usize, !(top as u32) as usize)
        }
    }

    #[inline(always)]
    fn search_uncached<const W: usize, const K: usize, const L: bool>(
        &self,
        d: &[u8],
        p: usize,
        window: usize,
        max: usize,
    ) -> (usize, usize) {
        let key = K;
        if p + key > d.len() {
            return (0, 0);
        }
        let bucket = Self::bucket::<W>(&self.t, self.h1k::<K>(d, p));
        if p + 8 <= d.len() {
            let mut best = Self::best_of(d, p, window, max, key, bucket, (0, 0));
            if L {
                let b2 = &self.t2[Self::h2(d, p, self.bits, self.lshift)];
                best = Self::best_of(d, p, window, max, key, b2, best);
            }
            return if best.0 < 3 { (0, 0) } else { best };
        }
        // The last few positions (no second table there).
        let (mut best, mut dist) = (0, 0);
        for &e in bucket {
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
            if d[c..c + key] != d[p..p + key] || (best > 0 && d[c + best] != d[p + best]) {
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
        if best < 3 { (0, 0) } else { (best, dist) }
    }
}

impl Cands for Core {
    fn entries(&mut self, d: &[u8], p: usize) -> Vec<usize> {
        self.ensure_any(d, p);
        if p + self.key > d.len() {
            return vec![];
        }
        let mut v: Vec<usize> = self
            .bucket_any(self.h1(d, p))
            .iter()
            .map(|&e| e as usize)
            .filter(|&e| e > 0 && e - 1 < p)
            .map(|e| e - 1)
            .collect();
        if self.long && p + 8 <= d.len() {
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

    fn index_of(&mut self, d: &[u8], p: usize, dist: usize) -> Option<usize> {
        if self.long || dist > p {
            return self.entries(d, p).iter().position(|&c| p - c == dist);
        }
        // One table: the entries before `p` in order, without collecting them.
        self.ensure_any(d, p);
        if p + self.key > d.len() {
            return None;
        }
        let target = (p - dist) as u32 + 1;
        self.bucket_any(self.h1(d, p))
            .iter()
            .filter(|&&e| e > 0 && (e as usize - 1) < p)
            .position(|&e| e == target)
    }
}

impl<const W: usize, const K: usize, const L: bool> Cands for Table<W, K, L> {
    fn entries(&mut self, d: &[u8], p: usize) -> Vec<usize> {
        self.0.entries(d, p)
    }

    fn index_of(&mut self, d: &[u8], p: usize, dist: usize) -> Option<usize> {
        self.0.index_of(d, p, dist)
    }

    fn ranked(&self) -> bool {
        self.0.ranked()
    }

    fn rank(&mut self, d: &[u8], p: usize, len: usize, dist: usize, limit: usize) -> Option<usize> {
        let (mut r, mut found) = (0, None);
        self.0.walk::<W, K, L>(d, p, limit, |c| {
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
        self.0.walk::<W, K, L>(d, p, limit, |c| {
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
}

impl<const W: usize, const K: usize, const L: bool> Search for Table<W, K, L> {
    #[inline(always)]
    fn search(&mut self, d: &[u8], p: usize, window: usize) -> (usize, usize) {
        let t = &mut self.0;
        let max = (d.len() - p).min(MAX_MATCH);
        if max < 3 {
            return (0, 0);
        }
        t.ensure::<W, K, L>(d, p);
        if (t.memo.0, t.memo.1) == (p, window) {
            return t.memo.2;
        }
        let r = t.search_uncached::<W, K, L>(d, p, window, max);
        t.memo = (p, window, r);
        r
    }

    #[inline(always)]
    fn peek(&mut self, d: &[u8], p: usize, window: usize) -> (usize, usize) {
        let max = (d.len() - p).min(MAX_MATCH);
        if max < 3 {
            return (0, 0);
        }
        self.0.search_uncached::<W, K, L>(d, p, window, max)
    }

    #[inline(always)]
    fn token_start(&mut self, d: &[u8], p: usize) {
        let t = &mut self.0;
        if t.inner.is_some() && t.last_start != p {
            t.insert::<W, K, L>(d, p);
            t.last_start = p;
        }
    }

    #[inline(always)]
    fn token(&mut self, d: &[u8], p: usize, len: usize) {
        let t = &mut self.0;
        let Some(inner) = t.inner else {
            return;
        };
        self.token_start(d, p);
        let t = &mut self.0;
        if len > 1 && len <= inner {
            t.memo.0 = usize::MAX;
            let fast = (p + len).min(d.len().saturating_sub(7)).max(p + 1);
            t.insert_run::<W, K, L>(d, p + 1, fast);
            for q in fast..p + len {
                t.insert::<W, K, L>(d, q);
            }
        }
    }
}
