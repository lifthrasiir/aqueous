//! Exact match candidates: for every position, the chain of earlier positions starting with
//! the same three bytes. Unlike an encoder's hash chain this has no collisions, so it does
//! not depend on any particular implementation's hash function.

pub const WINDOW: usize = 32768;
pub const MAX_MATCH: usize = 258;
const NONE: u32 = u32::MAX;

pub struct Chains<'a> {
    data: &'a [u8],
    prev: Vec<u32>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Found {
    /// 0 when there is no match of at least 3 bytes.
    pub len: usize,
    pub dist: usize,
}

impl<'a> Chains<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        let n = data.len();
        let mut prev = vec![NONE; n];
        if n >= 3 {
            // Positions + 1 by 24-bit key; zero-initialized so untouched pages cost nothing.
            let mut head = vec![0u32; 1 << 24];
            for i in 0..n - 2 {
                let k =
                    data[i] as usize | (data[i + 1] as usize) << 8 | (data[i + 2] as usize) << 16;
                prev[i] = head[k].wrapping_sub(1);
                head[k] = i as u32 + 1;
            }
        }
        Chains { data, prev }
    }

    #[inline]
    fn max_len(&self, p: usize) -> usize {
        (self.data.len() - p).min(MAX_MATCH)
    }

    /// Length of the common prefix of `data[a..]` and `data[p..]`, up to `max`.
    #[inline]
    fn common(&self, a: usize, p: usize, max: usize) -> usize {
        let d = self.data;
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

    /// Candidates of `p` within the window, closest first.
    #[inline]
    fn candidates(&self, p: usize) -> impl Iterator<Item = usize> + '_ {
        let mut c = if p < self.prev.len() {
            self.prev[p]
        } else {
            NONE
        };
        std::iter::from_fn(move || {
            if c == NONE || p - c as usize > WINDOW {
                return None;
            }
            let r = c as usize;
            c = self.prev[r];
            Some(r)
        })
    }

    /// The closest longest match among the first `depth` candidates, stopping early once a
    /// match reaches `nice`.
    pub fn longest(&self, p: usize, depth: usize, nice: usize) -> Found {
        let max = self.max_len(p);
        let mut best = Found::default();
        if max < 3 {
            return best;
        }
        for c in self.candidates(p).take(depth) {
            let d = self.data;
            if best.len > 0 && d[c + best.len] != d[p + best.len] {
                continue;
            }
            let l = self.common(c, p, max);
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

    /// Rank of distance `dist` among the candidates of `p` matching at least `len` bytes,
    /// closest first, or `None` if it is not among the first `limit` candidates.
    pub fn rank(&self, p: usize, len: usize, dist: usize, limit: usize) -> Option<usize> {
        let mut r = 0;
        for c in self.candidates(p).take(limit) {
            if p - c == dist {
                return Some(r);
            }
            if self.common(c, p, len) >= len {
                r += 1;
            }
        }
        None
    }

    /// Inverse of [`Chains::rank`].
    pub fn select(&self, p: usize, len: usize, rank: usize, limit: usize) -> Option<usize> {
        let mut r = 0;
        for c in self.candidates(p).take(limit) {
            if self.common(c, p, len) >= len {
                if r == rank {
                    return Some(p - c);
                }
                r += 1;
            }
        }
        None
    }

    /// Whether `data[p - dist..]` matches `data[p..]` for `len` bytes.
    pub fn valid(&self, p: usize, len: usize, dist: usize) -> bool {
        dist >= 1 && dist <= p && dist <= WINDOW && p + len <= self.data.len() && {
            (0..len).all(|i| self.data[p - dist + i] == self.data[p + i])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn longest_and_rank() {
        let data = b"abcdeXabcdYabcdeZabcQ";
        let ch = Chains::new(data);
        // At 17 ("abcQ"): candidates 11 ("abcdeZ"), 6 ("abcdY"), 0 ("abcdeX").
        assert_eq!(ch.longest(17, 100, 258), Found { len: 3, dist: 6 });
        assert_eq!(ch.longest(11, 100, 258), Found { len: 5, dist: 11 });
        assert_eq!(ch.longest(11, 1, 258), Found { len: 4, dist: 5 });
        assert_eq!(ch.rank(11, 4, 5, 100), Some(0));
        assert_eq!(ch.rank(11, 4, 11, 100), Some(1));
        assert_eq!(ch.rank(11, 5, 11, 100), Some(0));
        assert_eq!(ch.select(11, 4, 1, 100), Some(11));
        assert_eq!(ch.select(17, 3, 2, 100), Some(17));
    }
}
