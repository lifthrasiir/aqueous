//! Exact match candidates: for every position, the chain of earlier positions starting with
//! the same three bytes. Unlike an encoder's hash chain this has no collisions, so it does
//! not depend on any particular implementation's hash function.
//!
//! Searches go forward in a single pass (apart from a lookahead, or another pass from the
//! start): only the chains of the window behind the last search are kept.

pub const WINDOW: usize = 32768;
pub const MAX_MATCH: usize = 258;
const NONE: u32 = u32::MAX;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Found {
    /// 0 when there is no match of at least 3 bytes.
    pub len: usize,
    pub dist: usize,
}

pub struct Matcher {
    /// Previous position with the same three leading bytes.
    chain: Chain,
    /// The last two searches, (position, depth, nice, result).
    memo: [(usize, usize, usize, Found); 2],
}

/// The last position of every three-byte key so far, from which positions are linked.
enum Heads {
    /// Open addressing from keys to last positions, packed as key << 32 | position + 1 so
    /// that a probe touches one word; 0 marks a free slot. Sized for the data.
    Hash { table: Vec<u64>, bits: u32 },
    /// Position + 1 (0: none) by key: 64 MiB, which no hash table for that much data beats.
    Direct(Vec<u32>),
}

impl Heads {
    fn new(n: usize) -> Heads {
        if n > 1 << 22 {
            return Heads::Direct(vec![0; 1 << 24]);
        }
        // At most a quarter full (fewer probes), unless that takes more than 4M slots
        // (32 MiB); never more than half full.
        let keys = n.saturating_sub(2);
        let slots = (4 * keys).min(1 << 22).max(2 * keys);
        let bits = slots.next_power_of_two().max(1024).trailing_zeros();
        Heads::Hash {
            table: vec![0; 1 << bits],
            bits,
        }
    }

    fn clear(&mut self) {
        match self {
            Heads::Hash { table, .. } => table.fill(0),
            Heads::Direct(t) => t.fill(0),
        }
    }

    /// Calls `out(i, previous position with the same key)` for every `i` of `range` in order,
    /// making each the last position of its key.
    #[inline]
    fn link(&mut self, data: &[u8], range: std::ops::Range<usize>, out: impl FnMut(usize, u32)) {
        // One loop per kind of table, rather than a dispatch per position.
        match self {
            Heads::Direct(t) => link_with(data, range, out, |i, k| {
                std::mem::replace(&mut t[k as usize], i as u32 + 1).wrapping_sub(1)
            }),
            Heads::Hash { table, bits } => {
                let (bits, mask) = (*bits, table.len() - 1);
                link_with(data, range, out, |i, k| {
                    let mut h = (k.wrapping_mul(0x9e37_79b1) >> (32 - bits)) as usize;
                    let p = loop {
                        let e = table[h];
                        if e == 0 {
                            break NONE;
                        }
                        if (e >> 32) as u32 == k {
                            break e as u32 - 1;
                        }
                        h = (h + 1) & mask;
                    };
                    table[h] = (k as u64) << 32 | (i as u64 + 1);
                    p
                })
            }
        }
    }
}

/// [`Heads::link`] with `insert(i, key)` returning the previous position of `key`.
#[inline(always)]
fn link_with(
    data: &[u8],
    range: std::ops::Range<usize>,
    mut out: impl FnMut(usize, u32),
    mut insert: impl FnMut(usize, u32) -> u32,
) {
    // Keys by 4-byte loads while they fit, the rest byte by byte.
    let fast = range.end.min(data.len().saturating_sub(3)).max(range.start);
    for i in range.start..fast {
        let k = u32::from_le_bytes(data[i..i + 4].try_into().unwrap()) & 0xff_ffff;
        out(i, insert(i, k));
    }
    for i in fast..range.end {
        out(i, key(data, i).map_or(NONE, |k| insert(i, k)));
    }
}

/// The key of position `i`: its three leading bytes.
#[inline]
fn key(data: &[u8], i: usize) -> Option<u32> {
    let k = data.get(i..i + 3)?;
    Some(k[0] as u32 | (k[1] as u32) << 8 | (k[2] as u32) << 16)
}

/// Links kept for positions within this distance of the last one linked: the window, plus
/// the lookahead of a search behind it, plus the positions linked ahead of it.
const RING: usize = 1 << 17;

/// Positions linked past a search at once; [`RING`] covers them and the window.
const LINK_AHEAD: usize = 8192;

/// The chains near the positions searched last.
struct Chain {
    /// The link of position `base + i` is `base + links[i]`. Relative to `base` rather than
    /// modulo a ring size, so that a step along a chain is a single load: any arithmetic
    /// would add to its latency.
    links: Vec<u32>,
    base: usize,
    /// Positions `start..next` are linked, in a buffer of `2 * RING` that slides by `RING`
    /// (or one holding them all).
    heads: Heads,
    start: usize,
    next: usize,
    /// Searches from here on (and before `next`) find their window linked.
    lo_search: usize,
}

impl Chain {
    /// Links for searches at `start..end` only (plus lookahead).
    fn ring(data: &[u8], start: usize, end: usize) -> Chain {
        // Positions linked: the window before `start`, up to linking ahead of the last search.
        let first = start.saturating_sub(WINDOW);
        let span = (end + LINK_AHEAD + 1).min(data.len()) - first;
        Chain {
            // Never slides if it holds every position.
            links: vec![NONE; (2 * RING).min(data.len() + 1)],
            base: first,
            heads: Heads::new(span),
            start: first,
            next: first,
            lo_search: 0,
        }
    }

    /// Makes the links of `p` and of every position within the window before it available.
    #[inline(always)]
    fn ensure(&mut self, data: &[u8], p: usize) {
        // Usually linked already: the window behind `p` is still in the buffer.
        if p >= self.next || p < self.lo_search {
            self.extend(data, p);
        }
    }

    #[inline(never)]
    fn extend(&mut self, data: &[u8], p: usize) {
        if p < self.next {
            // A search too far back (another pass): link the window before it afresh.
            self.heads.clear();
            self.start = p.saturating_sub(WINDOW);
            self.next = self.start;
            self.base = self.start;
        }
        // Link ahead in batches: tight loops are faster, and links of later positions do not
        // affect searches from earlier ones.
        let end = (p + LINK_AHEAD).min(data.len()).max(p + 1);
        while self.next < end {
            if self.next == self.base + self.links.len() {
                // The window of every search from here on is in the second half.
                for i in 0..RING {
                    let l = self.links[RING + i];
                    self.links[i] = if l != NONE && l as usize >= RING {
                        l - RING as u32
                    } else {
                        NONE
                    };
                }
                self.base += RING;
            }
            let (base, stop) = (self.base, end.min(self.base + self.links.len()));
            let rel = |prev: u32| match prev {
                NONE => NONE,
                prev if (prev as usize) < base => NONE,
                prev => (prev as usize - base) as u32,
            };
            let links = &mut self.links[..stop - base];
            self.heads
                .link(data, self.next..stop, |i, prev| links[i - base] = rel(prev));
            self.next = stop;
        }
        let lo = self.start.max(self.base);
        self.lo_search = if lo == 0 { 0 } else { lo + WINDOW };
        debug_assert!(self.lo_search <= p && p < self.next);
    }
}

/// Length of the common prefix of `a` and `b`.
#[inline]
fn common_slices(a: &[u8], b: &[u8]) -> usize {
    let max = a.len().min(b.len());
    let (a, b) = (&a[..max], &b[..max]);
    let mut l = 0;
    for (x, y) in a.chunks_exact(8).zip(b.chunks_exact(8)) {
        let x = u64::from_le_bytes(x.try_into().unwrap());
        let y = u64::from_le_bytes(y.try_into().unwrap());
        if x != y {
            return l + ((x ^ y).trailing_zeros() / 8) as usize;
        }
        l += 8;
    }
    while l < max && a[l] == b[l] {
        l += 1;
    }
    l
}

/// Length of the common prefix of `d[a..]` and `d[p..]`, up to `max`.
#[inline]
fn common(d: &[u8], a: usize, p: usize, max: usize) -> usize {
    let mut l = 0;
    while l + 8 <= max {
        let x = u64::from_le_bytes(d[a + l..a + l + 8].try_into().unwrap());
        let y = u64::from_le_bytes(d[p + l..p + l + 8].try_into().unwrap());
        if x != y {
            return l + ((x ^ y).trailing_zeros() / 8) as usize;
        }
        l += 8;
    }
    while l < max && d[a + l] == d[p + l] {
        l += 1;
    }
    l
}

impl Matcher {
    /// A matcher over `data`: searches walk the chain directly, and only the last two are
    /// remembered (a literal's next position is searched again as the next token's).
    pub fn single(data: &[u8]) -> Self {
        Self::single_range(data, 0, data.len())
    }

    /// [`Matcher::single`] for searches at `start..end` only: its memory is that of the
    /// range rather than of the data.
    pub fn single_range(data: &[u8], start: usize, end: usize) -> Self {
        Matcher {
            chain: Chain::ring(data, start, end),
            memo: [(usize::MAX, 0, 0, Found::default()); 2],
        }
    }

    /// Walks the candidates of `p` within the window, closest first, calling
    /// `f(position, depth index)` until it returns false or the depth index reaches `limit`.
    #[inline]
    fn walk(&mut self, d: &[u8], p: usize, limit: usize, mut f: impl FnMut(usize, usize) -> bool) {
        self.chain.ensure(d, p);
        // In positions relative to base.
        let (links, base) = (&self.chain.links[..], self.chain.base);
        let p = p - base;
        let mut c = links.get(p).copied().unwrap_or(NONE);
        let mut idx = 0;
        while c != NONE && p - c as usize <= WINDOW && idx < limit {
            if !f(base + c as usize, idx) {
                return;
            }
            c = links[c as usize];
            idx += 1;
        }
    }

    /// The closest longest match among the first `depth` candidates of `data[p..]`, stopping
    /// early once a match reaches `nice`.
    pub fn longest(&mut self, d: &[u8], p: usize, depth: usize, nice: usize) -> Found {
        if let Some(m) = self
            .memo
            .iter()
            .find(|m| (m.0, m.1, m.2) == (p, depth, nice))
        {
            return m.3;
        }
        let max = (d.len() - p).min(MAX_MATCH);
        let mut best = Found::default();
        if max >= 3 {
            let dp = &d[p..p + max];
            // A candidate can only do better if it agrees with p up to best.len included:
            // checking its last two of those bytes first rejects most at once, as zlib does.
            let mut end = [0u8; 2];
            self.walk(d, p, depth, |c, _| {
                let dc = &d[c..c + max];
                if best.len > 0 && (dc[best.len] != end[1] || dc[best.len - 1] != end[0]) {
                    return true;
                }
                let l = common_slices(dc, dp);
                if l > best.len {
                    best = Found {
                        len: l,
                        dist: p - c,
                    };
                    if l >= nice || l >= max {
                        return false;
                    }
                    end = [dp[l - 1], dp[l]];
                }
                true
            });
        }
        self.memo[1] = self.memo[0];
        self.memo[0] = (p, depth, nice, best);
        best
    }

    /// The `q`-quantile (0..=1) of the chain indices of matches `(position, distance)`: how
    /// many closer candidates each match's source had. O(1) per match, by numbering positions
    /// within their chains in one pass; `matches` must come in order of position.
    pub fn depth_quantile(
        &mut self,
        data: &[u8],
        matches: impl Iterator<Item = (usize, usize)>,
        q: f64,
    ) -> usize {
        // Numbers of the last positions (a match's source is within the window), all of them
        // for short data.
        let mask = RING.min(data.len().next_power_of_two()) - 1;
        let mut seq = vec![0u32; mask + 1];
        let mut next = 0;
        let mut depths = vec![];
        for (p, dist) in matches {
            if p >= data.len() || dist > p || dist > WINDOW {
                continue;
            }
            while next <= p {
                self.chain.ensure(data, next);
                let (links, base) = (&self.chain.links, self.chain.base);
                let until = (p + 1).min(self.chain.next);
                for i in next..until {
                    let link = links[i - base];
                    let prev = base + link as usize;
                    seq[i & mask] = if link != NONE && i - prev <= WINDOW {
                        seq[prev & mask] + 1
                    } else {
                        0
                    };
                }
                next = until;
            }
            depths.push(seq[p & mask].saturating_sub(seq[(p - dist) & mask] + 1));
        }
        if depths.is_empty() {
            return 0;
        }
        let k = ((depths.len() - 1) as f64 * q) as usize;
        *depths.select_nth_unstable(k).1 as usize
    }

    /// Rank of distance `dist` among the candidates of `p` matching at least `len` bytes,
    /// closest first, or `None` if it is not among the first `limit` candidates.
    pub fn rank(
        &mut self,
        d: &[u8],
        p: usize,
        len: usize,
        dist: usize,
        limit: usize,
    ) -> Option<usize> {
        let (mut r, mut found) = (0, None);
        self.walk(d, p, limit, |c, _| {
            if p - c == dist {
                found = Some(r);
                return false;
            }
            if common(d, c, p, len) >= len {
                r += 1;
            }
            true
        });
        found
    }

    /// Inverse of [`Matcher::rank`].
    pub fn select(
        &mut self,
        d: &[u8],
        p: usize,
        len: usize,
        rank: usize,
        limit: usize,
    ) -> Option<usize> {
        let (mut r, mut found) = (0, None);
        let dp = &d[p..p + len];
        self.walk(d, p, limit, |c, _| {
            let dc = &d[c..c + len];
            if dc[len - 1] == dp[len - 1] && common_slices(dc, dp) >= len {
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

    /// Whether `d[p - dist..]` matches `d[p..]` for `len` bytes.
    pub fn valid(d: &[u8], p: usize, len: usize, dist: usize) -> bool {
        dist >= 1
            && dist <= p
            && dist <= WINDOW
            && p + len <= d.len()
            && (0..len).all(|i| d[p - dist + i] == d[p + i])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reference chains: the previous position of every position with the same three leading
    /// bytes, from a hash map.
    fn reference_chain(data: &[u8]) -> Vec<Option<usize>> {
        let mut last = std::collections::HashMap::new();
        (0..data.len())
            .map(|i| data.get(i..i + 3).and_then(|k| last.insert(k, i)))
            .collect()
    }

    /// The first `limit` candidates of `p` within the window, closest first.
    fn candidates(prev: &[Option<usize>], p: usize, limit: usize) -> Vec<usize> {
        let mut v = vec![];
        let mut c = prev[p];
        while let Some(x) = c.filter(|&x| p - x <= WINDOW && v.len() < limit) {
            v.push(x);
            c = prev[x];
        }
        v
    }

    fn reference_longest(
        d: &[u8],
        prev: &[Option<usize>],
        p: usize,
        depth: usize,
        nice: usize,
    ) -> Found {
        let max = (d.len() - p).min(MAX_MATCH);
        let mut best = Found::default();
        if max < 3 {
            return best;
        }
        for c in candidates(prev, p, depth) {
            let l = common(d, c, p, max);
            if l > best.len {
                best = Found {
                    len: l,
                    dist: p - c,
                };
                if l >= nice || l == max {
                    break;
                }
            }
        }
        best
    }

    fn reference_rank(
        d: &[u8],
        prev: &[Option<usize>],
        p: usize,
        len: usize,
        dist: usize,
        limit: usize,
    ) -> Option<usize> {
        let cands = candidates(prev, p, limit);
        let i = cands.iter().position(|&c| p - c == dist)?;
        Some(
            cands[..i]
                .iter()
                .filter(|&&c| common(d, c, p, len) >= len)
                .count(),
        )
    }

    fn lcg_data(n: usize, seed: u32, alphabet: &[u8]) -> Vec<u8> {
        let mut lcg = seed;
        (0..n)
            .map(|_| {
                lcg = lcg.wrapping_mul(1_103_515_245).wrapping_add(12345);
                alphabet[(lcg >> 16) as usize % alphabet.len()]
            })
            .collect()
    }

    #[test]
    fn longest_and_rank() {
        let data = b"abcdeXabcdYabcdeZabcQ";
        let mut m = Matcher::single(data);
        // At 17 ("abcQ"): candidates 11 ("abcdeZ"), 6 ("abcdY"), 0 ("abcdeX").
        assert_eq!(m.longest(data, 17, 100, 258), Found { len: 3, dist: 6 });
        assert_eq!(m.longest(data, 11, 100, 258), Found { len: 5, dist: 11 });
        assert_eq!(m.longest(data, 11, 1, 258), Found { len: 4, dist: 5 });
        assert_eq!(m.longest(data, 11, 100, 4), Found { len: 4, dist: 5 });
        assert_eq!(m.rank(data, 11, 4, 5, 100), Some(0));
        assert_eq!(m.rank(data, 11, 4, 11, 100), Some(1));
        assert_eq!(m.rank(data, 11, 5, 11, 100), Some(0));
        assert_eq!(m.select(data, 11, 4, 1, 100), Some(11));
        assert_eq!(m.select(data, 17, 3, 2, 100), Some(17));
    }

    /// Keeping only the chains near the last search gives the same answers as full chains,
    /// over data longer than the ring, in a second pass, and after jumping ahead.
    #[test]
    fn ring_matches_full_chain() {
        let data = lcg_data(3 * RING + 1000, 3, b"aabc");
        let prev = reference_chain(&data);
        let mut ring = Matcher::single(&data);
        let passes = [
            (0..data.len()).step_by(3).collect::<Vec<_>>(),
            (0..RING).step_by(5).chain(2 * RING..data.len()).collect(),
        ];
        for pass in passes {
            for p in pass {
                for q in [p, p + 1] {
                    if q >= data.len() {
                        continue;
                    }
                    let f = reference_longest(&data, &prev, q, 64, 258);
                    assert_eq!(ring.longest(&data, q, 64, 258), f, "q={q}");
                    if f.len >= 3 {
                        let r = reference_rank(&data, &prev, q, 3, f.dist, 64);
                        assert_eq!(ring.rank(&data, q, 3, f.dist, 64), r, "q={q}");
                        assert_eq!(ring.select(&data, q, 3, r.unwrap(), 64), Some(f.dist));
                    }
                }
            }
        }
    }

    /// A matcher for a range far into the data links only the window before the range (its
    /// table is sized for the range: linking from the start could fill it up).
    #[test]
    fn range_links_from_its_window() {
        let data = lcg_data(4 * RING, 5, b"aabc");
        let prev = reference_chain(&data);
        let (start, end) = (3 * RING, 3 * RING + 5000);
        let mut m = Matcher::single_range(&data, start, end);
        for q in (start..end).step_by(7) {
            let f = reference_longest(&data, &prev, q, 64, 258);
            assert_eq!(m.longest(&data, q, 64, 258), f, "q={q}");
        }
        assert_eq!(m.chain.start, start - WINDOW);
    }

    /// Every (depth, nice) gives the closest longest match among that many candidates.
    #[test]
    fn longest_obeys_depth_and_nice() {
        let data = lcg_data(20000, 1, b"aab");
        let prev = reference_chain(&data);
        let mut m = Matcher::single(&data);
        for p in (0..data.len()).step_by(7) {
            for depth in [16, 1, 64, 2, 5, 4096] {
                for nice in [3, 4, 8, 32, 258] {
                    assert_eq!(
                        m.longest(&data, p, depth, nice),
                        reference_longest(&data, &prev, p, depth, nice),
                        "p={p} depth={depth} nice={nice}"
                    );
                }
            }
        }
    }

    /// The depth quantile is the chain index of the matches' sources.
    #[test]
    fn depth_quantile_counts_closer_candidates() {
        let data = b"abcXabcYabcZabcQ";
        // At 12, the candidates are 8, 4, 0.
        let mut m = Matcher::single(data);
        let mut q = |dist| m.depth_quantile(data, [(12, dist)].into_iter(), 0.5);
        assert_eq!((q(4), q(8), q(12)), (0, 1, 2));
    }
}
