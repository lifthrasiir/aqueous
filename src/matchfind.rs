//! Exact match candidates: for every position, the chain of earlier positions starting with
//! the same three bytes. Unlike an encoder's hash chain this has no collisions, so it does
//! not depend on any particular implementation's hash function.
//!
//! Searches are cached per position as the list of strict improvements found while walking
//! the chain closest first ("records"). The result of a search with any depth limit and nice
//! length follows from that list, so one walk serves every predictor configuration and
//! repeated passes over the same data. Walks are resumable and only go as deep as some
//! configuration has asked for.

pub const WINDOW: usize = 32768;
pub const MAX_MATCH: usize = 258;
const NONE: u32 = u32::MAX;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Found {
    /// 0 when there is no match of at least 3 bytes.
    pub len: usize,
    pub dist: usize,
}

/// A strict improvement at chain index `idx`.
#[derive(Debug, Clone, Copy)]
struct Rec {
    idx: u16,
    len: u16,
    dist: u16,
}

/// Search state of one position.
#[derive(Debug, Clone, Copy)]
struct Slot {
    /// Records are `recs[start..start + count]`; `start` is `NONE` before the first search.
    start: u32,
    count: u16,
    best: u16,
    /// Next candidate to examine and its chain index; `next` is `NONE` once the walk is over.
    next: u32,
    idx: u32,
}

const UNSEARCHED: Slot = Slot {
    start: NONE,
    count: 0,
    best: 0,
    next: NONE,
    idx: 0,
};

/// Predicted literal/length and distance code lengths of a block.
type Trees = std::sync::Arc<(Vec<u8>, Vec<u8>)>;

pub struct Matcher {
    /// Previous position with the same three leading bytes.
    prev: Vec<u32>,
    /// Search state per position, when searches are cached (empty otherwise).
    slots: Vec<Slot>,
    /// Without a cache: the last two searches, (position, depth, nice, result).
    memo: [(usize, usize, usize, Found); 2],
    recs: Vec<Rec>,
    /// Predicted Huffman trees by (block start, token count): when deconstructing, a block's
    /// tokens are the same in every pass.
    trees: std::collections::HashMap<(usize, usize), Trees>,
}

/// For every position, the previous position with the same three leading bytes.
fn build_chain(data: &[u8]) -> Vec<u32> {
    let n = data.len();
    if n < 3 {
        return vec![NONE; n];
    }
    // Open addressing from keys to last positions, packed as key << 32 | position + 1 so that
    // a probe touches one word; 0 marks a free slot.
    // At most a quarter full (fewer probes), unless that takes more than 4M slots (32 MiB);
    // never more than half full.
    let slots = (4 * (n - 2)).min(1 << 22).max(2 * (n - 2));
    let bits = slots.next_power_of_two().max(1024).trailing_zeros();
    let mask = (1usize << bits) - 1;
    let mut table = vec![0u64; mask + 1];
    let mut prev = Vec::with_capacity(n);
    let mut insert = |i: usize, k: u32| {
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
    };
    // Keys by 4-byte loads while they fit, the last one byte by byte.
    for (i, w) in data.windows(4).enumerate() {
        let k = u32::from_le_bytes(w.try_into().unwrap()) & 0xff_ffff;
        prev.push(insert(i, k));
    }
    let i = n - 3;
    let k = data[i] as u32 | (data[i + 1] as u32) << 8 | (data[i + 2] as u32) << 16;
    prev.push(insert(i, k));
    prev.resize(n, NONE);
    prev
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
    /// A matcher caching every search, for several passes and configurations over `data`.
    pub fn new(data: &[u8]) -> Self {
        Matcher {
            slots: vec![UNSEARCHED; data.len() + 1],
            ..Self::single(data)
        }
    }

    /// A matcher for a single pass with one configuration (reconstruction): searches walk
    /// the chain directly, and only the last two are remembered (a literal's next position
    /// is searched again as the next token's).
    pub fn single(data: &[u8]) -> Self {
        Matcher {
            prev: build_chain(data),
            slots: vec![],
            memo: [(usize::MAX, 0, 0, Found::default()); 2],
            recs: vec![],
            trees: Default::default(),
        }
    }

    /// Walks the candidates of `p` within the window, closest first, calling
    /// `f(position, depth index)` until it returns false or the depth index reaches `limit`.
    #[inline]
    fn walk(&self, p: usize, limit: usize, mut f: impl FnMut(usize, usize) -> bool) {
        let mut c = self.prev.get(p).copied().unwrap_or(NONE);
        let mut idx = 0;
        while c != NONE && p - c as usize <= WINDOW && idx < limit {
            if !f(c as usize, idx) {
                return;
            }
            c = self.prev[c as usize];
            idx += 1;
        }
    }

    /// The closest longest match among the first `depth` candidates of `data[p..]`, stopping
    /// early once a match reaches `nice`.
    pub fn longest(&mut self, d: &[u8], p: usize, depth: usize, nice: usize) -> Found {
        if self.slots.is_empty() {
            return self.longest_direct(d, p, depth, nice);
        }
        let mut slot = self.slots[p];
        if slot.start == NONE {
            let max = (d.len() - p).min(MAX_MATCH);
            slot = Slot {
                start: self.recs.len() as u32,
                next: if max >= 3 {
                    self.prev.get(p).copied().unwrap_or(NONE)
                } else {
                    NONE
                },
                ..UNSEARCHED
            };
        }
        // What the walk so far determines: the answer is final once a record reaches `nice`,
        // or the walk has covered `depth` candidates or ended.
        let answer = |recs: &[Rec]| {
            let mut best = Found::default();
            for r in recs {
                if r.idx as usize >= depth {
                    return Some(best);
                }
                best = Found {
                    len: r.len as usize,
                    dist: r.dist as usize,
                };
                if best.len >= nice {
                    return Some(best);
                }
            }
            None
        };
        let have = &self.recs[slot.start as usize..][..slot.count as usize];
        if let Some(f) = answer(have) {
            self.slots[p] = slot;
            return f;
        }
        if slot.next != NONE && (slot.idx as usize) < depth {
            // Resume the walk; the records must stay contiguous, so move them to the end.
            if slot.start as usize + slot.count as usize != self.recs.len() {
                let start = slot.start as usize;
                self.recs
                    .extend_from_within(start..start + slot.count as usize);
                slot.start = (self.recs.len() - slot.count as usize) as u32;
            }
            let max = (d.len() - p).min(MAX_MATCH);
            let mut best = slot.best as usize;
            let (mut c, mut idx) = (slot.next, slot.idx as usize);
            while c != NONE && p - c as usize <= WINDOW && idx < depth {
                let cu = c as usize;
                if best == 0 || d[cu + best] == d[p + best] {
                    let l = common(d, cu, p, max);
                    if l > best {
                        best = l;
                        self.recs.push(Rec {
                            idx: idx as u16,
                            len: l as u16,
                            dist: (p - cu) as u16,
                        });
                        slot.count += 1;
                        if l == max {
                            c = NONE;
                            break;
                        }
                    }
                }
                c = self.prev[cu];
                idx += 1;
            }
            if c != NONE && p - c as usize > WINDOW {
                c = NONE;
            }
            slot.best = best as u16;
            slot.next = c;
            slot.idx = idx as u32;
        }
        self.slots[p] = slot;
        let have = &self.recs[slot.start as usize..][..slot.count as usize];
        // Either the walk covers `depth` now or it is over: the last record within depth.
        answer(have).unwrap_or_else(|| {
            have.last().map_or(Found::default(), |r| Found {
                len: r.len as usize,
                dist: r.dist as usize,
            })
        })
    }

    /// [`crate::predict::predict_trees`] of the block of `tokens` starting at `start`, memoized.
    pub(crate) fn trees(&mut self, start: usize, tokens: &[crate::deflate::Token]) -> Trees {
        // A single pass never asks twice.
        if self.slots.is_empty() {
            return std::sync::Arc::new(crate::predict::predict_trees(tokens));
        }
        self.trees
            .entry((start, tokens.len()))
            .or_insert_with(|| std::sync::Arc::new(crate::predict::predict_trees(tokens)))
            .clone()
    }

    /// The `q`-quantile (0..=1) of the chain indices of matches `(position, distance)`: how
    /// many closer candidates each match's source had. O(1) per match, by numbering positions
    /// within their chains.
    pub fn depth_quantile(&self, matches: impl Iterator<Item = (usize, usize)>, q: f64) -> usize {
        let mut seq = vec![0u32; self.prev.len()];
        for i in 0..self.prev.len() {
            if self.prev[i] != NONE {
                seq[i] = seq[self.prev[i] as usize] + 1;
            }
        }
        let mut depths: Vec<u32> = matches
            .filter(|&(p, dist)| p < seq.len() && dist <= p)
            .map(|(p, dist)| seq[p].saturating_sub(seq[p - dist] + 1))
            .collect();
        if depths.is_empty() {
            return 0;
        }
        let k = ((depths.len() - 1) as f64 * q) as usize;
        *depths.select_nth_unstable(k).1 as usize
    }

    fn longest_direct(&mut self, d: &[u8], p: usize, depth: usize, nice: usize) -> Found {
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
            self.walk(p, depth, |c, _| {
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

    /// Rank of distance `dist` among the candidates of `p` matching at least `len` bytes,
    /// closest first, or `None` if it is not among the first `limit` candidates.
    pub fn rank(&self, d: &[u8], p: usize, len: usize, dist: usize, limit: usize) -> Option<usize> {
        let (mut r, mut found) = (0, None);
        self.walk(p, limit, |c, _| {
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
        &self,
        d: &[u8],
        p: usize,
        len: usize,
        rank: usize,
        limit: usize,
    ) -> Option<usize> {
        let (mut r, mut found) = (0, None);
        let dp = &d[p..p + len];
        self.walk(p, limit, |c, _| {
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

    #[test]
    fn longest_and_rank() {
        let data = b"abcdeXabcdYabcdeZabcQ";
        let mut m = Matcher::new(data);
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

    /// The cached records answer every (depth, nice) exactly like a direct search.
    #[test]
    fn records_match_direct_search() {
        let mut lcg = 1u32;
        let data: Vec<u8> = (0..20000)
            .map(|_| {
                lcg = lcg.wrapping_mul(1_103_515_245).wrapping_add(12345);
                b"aab"[(lcg >> 16) as usize % 3]
            })
            .collect();
        let mut m = Matcher::new(&data);
        let mut single = Matcher::single(&data);
        for p in (0..data.len()).step_by(7) {
            for depth in [16, 1, 64, 2, 5, 4096] {
                for nice in [3, 4, 8, 32, 258] {
                    // Direct search, as the predictor was originally written.
                    let max = (data.len() - p).min(MAX_MATCH);
                    let mut best = Found::default();
                    if max >= 3 {
                        let mut cands = vec![];
                        m.walk(p, depth, |c, _| {
                            cands.push(c);
                            true
                        });
                        for c in cands {
                            let l = common(&data, c, p, max);
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
                    }
                    assert_eq!(single.longest(&data, p, depth, nice), best, "single p={p}");
                    assert_eq!(
                        m.longest(&data, p, depth, nice),
                        best,
                        "p={p} depth={depth} nice={nice}"
                    );
                }
            }
        }
    }
}
