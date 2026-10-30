//! A match finder over the data: the nearest earlier position matching for a length, and the
//! longest match among the nearest few. Positions are added in order, so that a query at `pos`
//! sees only what lies before it, as when the stream was made, and no farther back than the
//! stream's dictionary allows (the chains are rings of that size).

const NONE: u32 = u32::MAX;
const HASH_BITS: u32 = 20;
/// Positions by a hash of their next 3 bytes: a bucket's newest position, where its bytes are
/// those, is their newest occurrence (as with LZMA's encoders, which look no further).
const HASH3_BITS: u32 = 16;
/// The key of the chain of matches of medium length.
pub const MID: usize = 8;
/// The key of the chain of long matches.
pub const LONG: usize = 16;

/// Positions by a hash of their next `K` bytes, the newest first.
struct Chain<const K: usize> {
    head: Vec<u32>,
    /// The position before each in its chain, by position modulo the ring's size: as many as
    /// there are positions where the data is shorter than the ring.
    prev: Vec<u32>,
    mask: usize,
    /// How far back a candidate may be.
    reach: usize,
    /// The matches [`Mf::longest`] went through on this chain, each longer than those nearer
    /// (its length and position).
    found: Vec<(u32, u32)>,
}

impl<const K: usize> Chain<K> {
    fn new(ring: usize, len: usize, reach: usize) -> Self {
        let prev = vec![NONE; ring.min(len.max(1))];
        Chain { head: vec![NONE; 1 << HASH_BITS], prev, mask: ring - 1, reach, found: vec![] }
    }

    #[inline]
    fn hash(d: &[u8], p: usize) -> usize {
        let h = if K == 4 {
            u32::from_le_bytes(d[p..p + 4].try_into().unwrap()) as u64
        } else if K == 8 {
            u64::from_le_bytes(d[p..p + 8].try_into().unwrap())
        } else {
            let a = u64::from_le_bytes(d[p..p + 8].try_into().unwrap());
            let b = u64::from_le_bytes(d[p + K - 8..p + K].try_into().unwrap());
            a ^ b.rotate_left(29).wrapping_mul(0xff51_afd7_ed55_8ccd)
        };
        (h.wrapping_mul(0x9e37_79b9_7f4a_7c15) >> (64 - HASH_BITS)) as usize
    }

    #[inline]
    fn insert(&mut self, d: &[u8], p: usize) {
        if p + K <= d.len() {
            let h = Self::hash(d, p);
            self.prev[p & self.mask] = self.head[h];
            self.head[h] = p as u32;
        }
    }

    /// The candidates for `pos`, nearest first, within reach.
    #[inline]
    fn walk(&self, d: &[u8], pos: usize) -> impl Iterator<Item = usize> {
        let mut c = self.head[Self::hash(d, pos)];
        std::iter::from_fn(move || {
            if c == NONE || pos - c as usize > self.reach {
                return None;
            }
            let cu = c as usize;
            c = self.prev[cu & self.mask];
            Some(cu)
        })
    }

    /// The nearest candidate matching `n` bytes, among `limit`.
    fn nearest(&self, d: &[u8], pos: usize, n: usize, limit: usize) -> Option<usize> {
        self.walk(d, pos).take(limit).find(|&c| d[c + n - 1] == d[pos + n - 1] && lcp(d, c, pos, n) >= n)
    }

    /// Goes through `depth` candidates for matches longer than `best`, up to `nice`, keeping
    /// them in `found`. Returns the longest, or 0 for none.
    fn longest(&mut self, d: &[u8], pos: usize, depth: usize, nice: usize, cap: usize, mut best: usize) -> usize {
        let mut found = std::mem::take(&mut self.found);
        found.clear();
        if pos + K <= d.len() {
            for c in self.walk(d, pos).take(depth) {
                // A longer match must agree at `best` first.
                if best < cap && d[c + best] == d[pos + best] {
                    let n = lcp(d, c, pos, cap);
                    if n > best {
                        best = n;
                        found.push((n as u32, c as u32));
                        if n >= nice {
                            break;
                        }
                    }
                }
            }
        }
        let r = if found.is_empty() { 0 } else { best };
        self.found = found;
        r
    }
}

#[inline]
fn k2(d: &[u8], p: usize) -> usize {
    d[p] as usize | (d[p + 1] as usize) << 8
}

#[inline]
fn k3(d: &[u8], p: usize) -> usize {
    k2(d, p) | (d[p + 2] as usize) << 16
}

#[inline]
fn h3(d: &[u8], p: usize) -> usize {
    ((k3(d, p) as u32).wrapping_mul(0x9e37_79b1) >> (32 - HASH3_BITS)) as usize
}

/// The length of the common prefix of `d[a..]` and `d[b..]`, `a < b`, at most `cap`.
#[inline]
pub fn lcp(d: &[u8], a: usize, b: usize, cap: usize) -> usize {
    let cap = cap.min(d.len() - b);
    let mut n = 0;
    while n + 8 <= cap {
        let x = u64::from_le_bytes(d[a + n..a + n + 8].try_into().unwrap())
            ^ u64::from_le_bytes(d[b + n..b + n + 8].try_into().unwrap());
        if x != 0 {
            return n + (x.trailing_zeros() / 8) as usize;
        }
        n += 8;
    }
    while n < cap && d[a + n] == d[b + n] {
        n += 1;
    }
    n
}

pub struct Mf<'a> {
    d: &'a [u8],
    /// How far back a match may be: `pos - c` at most this.
    reach: usize,
    head2: Vec<u32>,
    head3: Vec<u32>,
    short: Chain<4>,
    mid: Chain<MID>,
    long: Chain<LONG>,
    next: usize,
    /// The position the chains' `found` are for.
    found_at: usize,
}

impl<'a> Mf<'a> {
    /// A match finder for matches at most `reach` back (LZMA's dictionary size).
    pub fn new(d: &'a [u8], reach: usize) -> Self {
        // A ring of `reach` positions at least: a candidate `c` is at least `pos - reach`, and its
        // slot is taken again only by `c + ring`, which is at least `pos` (not added yet). So a
        // dictionary of 2^n bytes takes chains of 2^n, not 2^(n+1).
        let ring = reach.clamp(1, d.len().max(1)).next_power_of_two();
        Mf {
            d,
            reach,
            head2: vec![NONE; 1 << 16],
            head3: vec![NONE; 1 << HASH3_BITS],
            short: Chain::new(ring, d.len(), reach),
            mid: Chain::new(ring, d.len(), reach),
            long: Chain::new(ring, d.len(), reach),
            next: 0,
            found_at: usize::MAX,
        }
    }

    /// Adds the positions before `pos`.
    pub fn advance(&mut self, pos: usize) {
        let d = self.d;
        for p in self.next..pos {
            self.short.insert(d, p);
            self.mid.insert(d, p);
            self.long.insert(d, p);
            if p + 3 <= d.len() {
                self.head3[h3(d, p)] = p as u32;
            }
            if p + 2 <= d.len() {
                self.head2[k2(d, p)] = p as u32;
            }
        }
        self.next = self.next.max(pos);
    }

    /// The newest position with the same `k` (2 or 3) bytes as `pos`, within reach.
    #[inline]
    fn head(&self, k: usize, pos: usize) -> Option<usize> {
        let d = self.d;
        if pos + k > d.len() {
            return None;
        }
        let c = if k == 2 { self.head2[k2(d, pos)] } else { self.head3[h3(d, pos)] };
        (c != NONE && pos - c as usize <= self.reach && (k == 2 || k3(d, c as usize) == k3(d, pos)))
            .then_some(c as usize)
    }

    /// The nearest position before `pos` whose next `n` bytes are those at `pos`, looking at
    /// no more than `limit` candidates of a chain. Free up to what [`Mf::longest`] found at
    /// `pos`.
    pub fn nearest(&self, pos: usize, n: usize, limit: usize) -> Option<usize> {
        let d = self.d;
        if pos + n > d.len() || n < 2 {
            return None;
        }
        if n < 4 {
            return self.head(n, pos);
        }
        // Each chain is gone through nearest first, so the first match found as long is the
        // nearest.
        if self.found_at == pos {
            for found in [&self.short.found, &self.long.found] {
                if let Some(&(_, c)) = found.iter().find(|&&(l, _)| l as usize >= n) {
                    return Some(c as usize);
                }
            }
        }
        if n >= LONG {
            self.long.nearest(d, pos, n, limit)
        } else if n >= MID {
            self.mid.nearest(d, pos, n, limit)
        } else {
            self.short.nearest(d, pos, n, limit)
        }
    }

    /// After [`Mf::longest`] at `pos`: the matches there, each longer than all those nearer
    /// (their lengths and positions, at most `cap` long), as a binary-tree match finder gives
    /// them to an optimal parser. A length up to one's is nearest at its position.
    pub fn pairs(&self, pos: usize, cap: usize, out: &mut Vec<(u32, u32)>) {
        out.clear();
        out.extend(self.short.found.iter().chain(&self.long.found));
        for k in [2, 3] {
            if let Some(c) = self.head(k, pos) {
                out.push((lcp(self.d, c, pos, cap) as u32, c as u32));
            }
        }
        out.sort_unstable_by_key(|&(_, c)| std::cmp::Reverse(c));
        let mut top = 0;
        out.retain(|&(l, _)| {
            let keep = l > top;
            top = top.max(l);
            keep
        });
    }

    /// The length of the match at the newest candidate of 4 bytes (at most `cap`), or 2 or 3
    /// for the newest of 2 or 3 bytes: a cheap hint of what `pos` offers.
    pub fn first(&self, pos: usize, cap: usize) -> usize {
        let d = self.d;
        if pos + 4 <= d.len()
            && let Some(c) = self.short.walk(d, pos).next()
        {
            let n = lcp(d, c, pos, cap);
            if n >= 4 {
                return n;
            }
        }
        if self.head(3, pos).is_some() {
            3
        } else if self.head(2, pos).is_some() {
            2
        } else {
            0
        }
    }

    /// The longest match at `pos` (at most `cap`) among the nearest 2- and 3-byte matches,
    /// `depth` candidates of 4 bytes and `depth` of 16, stopping at one of `nice` bytes. A query
    /// of [`Mf::nearest`] at `pos` then costs nothing up to that length.
    pub fn longest(&mut self, pos: usize, depth: usize, nice: usize, cap: usize) -> usize {
        let d = self.d;
        self.found_at = pos;
        let mut best = if self.head(3, pos).is_some() {
            3
        } else if self.head(2, pos).is_some() {
            2
        } else {
            0
        };
        best = best.max(self.short.longest(d, pos, depth, nice, cap, best));
        if best < nice && best < cap {
            best = best.max(self.long.longest(d, pos, depth, nice, cap, LONG - 1));
        } else {
            self.long.found.clear();
        }
        best.min(cap)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Chains of as many positions as a dictionary of 2^n bytes, which still find the nearest
    /// match within it, at its very end too (data of a few bytes, longer than the dictionary).
    #[test]
    fn rings() {
        const REACH: usize = 4096;
        let mut r = aqr_common::rng::Rng::new(3);
        let d: Vec<u8> = (0..4 * REACH).map(|_| b"ab"[r.below(2) as usize]).collect();
        let mut mf = Mf::new(&d, REACH);
        assert_eq!([mf.short.prev.len(), mf.mid.prev.len(), mf.long.prev.len()], [REACH; 3]);
        for pos in (REACH..d.len() - LONG).step_by(7) {
            mf.advance(pos);
            for n in [4, MID, LONG] {
                let want = (pos - REACH..pos).rev().find(|&c| lcp(&d, c, pos, n) >= n);
                assert_eq!(mf.nearest(pos, n, usize::MAX), want, "{pos} {n}");
            }
        }
    }
}
