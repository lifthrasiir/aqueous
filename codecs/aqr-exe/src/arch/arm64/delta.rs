//! A stream of ARM64 code ([`super::CODE`], as [`super::remap`] writes it) against the same
//! stream of another version (bsdiff's idea, after Colin Percival's "Naive differences of
//! executable code"): copies of the reference's words at some offset, each word coded as its
//! difference from a prediction, and bytes of the stream's own between the copies.
//!
//! A word of the reference is predicted to become what it last became (data moved within a
//! page changes every instruction addressing it alike), in a table of [`TABLE_BITS`] bits by
//! a hash of the word. The differences are mostly 0, and go sparse: the run of words of 0
//! before each other one, and that word.
//!
//! The streams (`exe-recons/arm64-delta`, FORMAT.md §D), after the reference:
//!
//! - [`CTRL`]: of each copy, the bytes of the stream's own before it, the change of the offset
//!   from the last copy's (zigzag; the first from 0) and the words copied, as varints; then the
//!   bytes of its own after the last copy.
//! - [`GAPS`]: before each difference not 0, the differences of 0 before it, as varints.
//! - [`VALUES`]: the differences not 0, big-endian words (as the words are).
//! - [`EXTRA`]: the bytes of the stream's own.

use alloc::vec::Vec;

/// Number of the streams, after the reference.
pub const NUM_STREAMS: usize = 4;
pub const CTRL: usize = 0;
pub const GAPS: usize = 1;
pub const VALUES: usize = 2;
pub const EXTRA: usize = 3;

/// Names of the streams, in order.
pub const STREAM_NAMES: [&str; NUM_STREAMS] = ["CTRL", "GAPS", "VALUES", "EXTRA"];

/// Bits of the table of predictions (8 bytes an entry).
pub const TABLE_BITS: u32 = 16;

fn word(d: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(*d.get(at..)?.first_chunk()?))
}

/// What each word of the reference last became, by a hash of the word: one entry a slot, the
/// last kept. (An entry of 0 for 0 stands for none.)
struct Predict(Vec<(u32, u32)>);

impl Predict {
    fn new() -> Predict {
        Predict(alloc::vec![(0, 0); 1 << TABLE_BITS])
    }

    fn slot(x: u32) -> usize {
        (x.wrapping_mul(0x9E37_79B1) >> (32 - TABLE_BITS)) as usize
    }

    fn get(&self, x: u32) -> u32 {
        match self.0[Self::slot(x)] {
            (k, v) if k == x => v,
            _ => x,
        }
    }

    /// `x` became `y` (a word copied as it is takes no slot, unless its own).
    fn set(&mut self, x: u32, y: u32) {
        let e = &mut self.0[Self::slot(x)];
        if x != y || e.0 == x {
            *e = (x, y);
        }
    }
}

#[cfg(feature = "decode")]
fn varint(s: &mut &[u8]) -> Option<u64> {
    let mut v = 0u64;
    for k in 0..10 {
        let (&x, rest) = s.split_first()?;
        *s = rest;
        v |= ((x & 0x7f) as u64) << (7 * k);
        if x < 0x80 {
            return Some(v);
        }
    }
    None
}

/// The stream of `len` bytes from `streams` against `reference`, or `None` where they are not
/// of one.
#[cfg(feature = "decode")]
pub fn decode(reference: &[u8], streams: [&[u8]; NUM_STREAMS], len: usize) -> Option<Vec<u8>> {
    let [mut ctrl, mut gaps, mut values, mut extra] = streams;
    let mut out = Vec::with_capacity(len);
    let mut own = |out: &mut Vec<u8>, n: u64| -> Option<()> {
        let n = usize::try_from(n).ok()?;
        let (e, rest) = (extra.get(..n)?, extra.get(n..)?);
        out.extend_from_slice(e);
        extra = rest;
        Some(())
    };
    let mut pr = Predict::new();
    // the words of 0 before the next not 0 (u64::MAX: none left, all 0)
    let next_gap = |gaps: &mut &[u8]| if gaps.is_empty() { Some(u64::MAX) } else { varint(gaps) };
    let (mut off, mut zeros) = (0i64, next_gap(&mut gaps)?);
    loop {
        own(&mut out, varint(&mut ctrl)?)?;
        if ctrl.is_empty() {
            break;
        }
        let z = varint(&mut ctrl)?;
        off = off.checked_add((z >> 1) as i64 ^ -((z & 1) as i64))?;
        let words = varint(&mut ctrl)?;
        if words > (len.saturating_sub(out.len()) / 4) as u64 {
            return None;
        }
        for _ in 0..words {
            let at = usize::try_from(i64::try_from(out.len()).ok()?.checked_add(off)?).ok()?;
            let x = word(reference, at)?;
            let d = if zeros == 0 {
                let (v, rest) = values.split_first_chunk::<4>()?;
                values = rest;
                zeros = next_gap(&mut gaps)?;
                u32::from_be_bytes(*v)
            } else {
                zeros -= (zeros != u64::MAX) as u64;
                0
            };
            let y = d.wrapping_add(pr.get(x));
            pr.set(x, y);
            out.extend_from_slice(&y.to_be_bytes());
        }
    }
    (out.len() == len && extra.is_empty() && values.is_empty() && zeros == u64::MAX).then_some(out)
}

#[cfg(feature = "encode")]
mod encode {
    use super::*;

    /// Bytes of a word: matches are of whole words, at word boundaries.
    const WORD: usize = 4;
    /// Words hashed for a match.
    const SEED: usize = 4;
    /// Exact matches shorter than this (in words) are not taken as anchors.
    const MIN_MATCH: usize = 6;
    /// Copies of one offset at most this many words apart are one, the words between them
    /// differences.
    const MERGE_GAP: usize = 16;
    /// Positions in each bucket of the index.
    const WAYS: usize = 4;

    fn put(out: &mut Vec<u8>, mut v: u64) {
        while v >= 0x80 {
            out.push(v as u8 | 0x80);
            v >>= 7;
        }
        out.push(v as u8);
    }

    fn w(d: &[u8], i: usize) -> u32 {
        u32::from_be_bytes(*d[i * WORD..].first_chunk().unwrap())
    }

    fn hash(d: &[u8], i: usize, bits: u32) -> usize {
        let mut h = 0u64;
        for k in 0..SEED {
            h = (h ^ w(d, i + k) as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        }
        (h >> (64 - bits)) as usize
    }

    /// Positions of the words of a stream by a hash of the [`SEED`] words from each (the
    /// last [`WAYS`] of each bucket), or of those whose hash's low `sample` bits are 0.
    struct Index {
        bits: u32,
        sample: u32,
        table: Vec<u32>,
    }

    impl Index {
        fn new(a: &[u8], sample: u32) -> Index {
            let n = (a.len() / WORD).saturating_sub(SEED - 1);
            let bits = (usize::BITS - (n >> sample).max(1 << 10).leading_zeros()).min(28);
            let mut table = alloc::vec![u32::MAX; (1 << bits) * WAYS];
            let mut fill = alloc::vec![0u8; 1 << bits];
            let mask = (1 << sample) - 1;
            for i in 0..n {
                let h = hash(a, i, bits + sample);
                if h & mask != 0 {
                    continue;
                }
                let h = h >> sample;
                table[h * WAYS + fill[h] as usize % WAYS] = i as u32;
                fill[h] = fill[h].wrapping_add(1);
            }
            Index { bits, sample, table }
        }

        /// The positions whose words may match those of `b` from word `i`.
        fn at(&self, b: &[u8], i: usize) -> Option<&[u32]> {
            let h = hash(b, i, self.bits + self.sample);
            if h & ((1 << self.sample) - 1) != 0 {
                return None;
            }
            let h = h >> self.sample;
            Some(&self.table[h * WAYS..(h + 1) * WAYS])
        }
    }

    /// A copy: words [b, b + len) against the reference's from b + off (in words).
    #[derive(Clone, Copy, Debug)]
    struct Copy {
        b: usize,
        len: usize,
        off: i64,
    }

    /// Exact matches of whole words, greedily from the start, each from the last offset or
    /// one that the index has.
    fn anchors(a: &[u8], b: &[u8], idx: &Index) -> Vec<Copy> {
        let (na, nb) = (a.len() / WORD, b.len() / WORD);
        let mut out: Vec<Copy> = Vec::new();
        let (mut i, mut last) = (0, None::<i64>);
        let mut cands = Vec::with_capacity(WAYS + 1);
        while i + SEED <= nb {
            cands.clear();
            cands.extend(last);
            if let Some(ps) = idx.at(b, i) {
                cands.extend(ps.iter().filter(|&&p| p != u32::MAX).map(|&p| p as i64 - i as i64));
            }
            let floor = out.last().map_or(0, |c| c.b + c.len);
            let mut best = (0, 0i64, 0);
            for &off in &cands {
                let j = i as i64 + off;
                if j < 0 || j as usize >= na {
                    continue;
                }
                let j = j as usize;
                let mut f = 0;
                while i + f < nb && j + f < na && w(b, i + f) == w(a, j + f) {
                    f += 1;
                }
                if f == 0 {
                    continue;
                }
                let mut k = 0;
                while i - k > floor && j > k && w(b, i - k - 1) == w(a, j - k - 1) {
                    k += 1;
                }
                if f + k > best.0 {
                    best = (f + k, off, k);
                }
            }
            if best.0 >= MIN_MATCH {
                let start = i - best.2;
                out.push(Copy { b: start, len: best.0, off: best.1 });
                last = Some(best.1);
                i = start + best.0;
            } else {
                i += 1;
            }
        }
        out
    }

    /// The anchors extended over approximate matches into the gaps between them: each
    /// backwards to the end of the one before (extended), then forwards to the next anchor, as
    /// far as the words alike outnumber the others the most; those of one offset close enough
    /// merged.
    fn extend(a: &[u8], b: &[u8], anchors: &[Copy]) -> Vec<Copy> {
        let (na, nb) = (a.len() / WORD, b.len() / WORD);
        let alike = |p: usize, off: i64| {
            let j = p as i64 + off;
            j >= 0 && (j as usize) < na && w(a, j as usize) == w(b, p)
        };
        let mut out: Vec<Copy> = Vec::with_capacity(anchors.len());
        for (k, c) in anchors.iter().enumerate() {
            let (mut s, mut e) = (c.b, c.b + c.len);
            let lo = out.last().map_or(0, |p| p.b + p.len);
            let (mut score, mut best, mut len) = (0i64, 0i64, 0);
            for p in (lo..s).rev() {
                score += if alike(p, c.off) { 1 } else { -1 };
                if score > best {
                    (best, len) = (score, s - p);
                }
            }
            s -= len;
            let hi = anchors.get(k + 1).map_or(nb, |n| n.b);
            let (mut score, mut best, mut len) = (0i64, 0i64, 0);
            for p in e..hi {
                score += if alike(p, c.off) { 1 } else { -1 };
                if score > best {
                    (best, len) = (score, p + 1 - e);
                }
            }
            e += len;
            match out.last_mut() {
                Some(p) if p.off == c.off && s - (p.b + p.len) <= MERGE_GAP => p.len = e - p.b,
                _ => out.push(Copy { b: s, len: e - s, off: c.off }),
            }
        }
        out
    }

    /// The streams of `b` against `reference` ([`decode`] makes `b` of them).
    pub fn encode(reference: &[u8], b: &[u8]) -> [Vec<u8>; NUM_STREAMS] {
        let copies = extend(reference, b, &anchors(reference, b, &Index::new(reference, 0)));
        let [mut ctrl, mut gaps, mut values, mut extra] = core::array::from_fn(|_| Vec::new());
        let (mut pos, mut last_off, mut zeros) = (0usize, 0i64, 0u64);
        let mut pr = Predict::new();
        for c in &copies {
            put(&mut ctrl, ((c.b - pos) * WORD) as u64);
            extra.extend_from_slice(&b[pos * WORD..c.b * WORD]);
            let d = (c.off - last_off) * WORD as i64;
            put(&mut ctrl, ((d << 1) ^ (d >> 63)) as u64);
            put(&mut ctrl, c.len as u64);
            for i in c.b..c.b + c.len {
                let (x, y) = (w(reference, (i as i64 + c.off) as usize), w(b, i));
                let d = y.wrapping_sub(pr.get(x));
                pr.set(x, y);
                if d == 0 {
                    zeros += 1;
                } else {
                    put(&mut gaps, zeros);
                    values.extend_from_slice(&d.to_be_bytes());
                    zeros = 0;
                }
            }
            (pos, last_off) = (c.b + c.len, c.off);
        }
        put(&mut ctrl, (b.len() - pos * WORD) as u64);
        extra.extend_from_slice(&b[pos * WORD..]);
        [ctrl, gaps, values, extra]
    }

    /// Of `b` cut into pieces of `piece` bytes, the pieces of `reference` (cut into pieces of
    /// `of`) that each is to be coded against: the `most` that the longest of its exact matches
    /// fall in (by the words matched), in order; none where it matches nothing. The matches are
    /// those [`encode`] starts from, looked for at a sample of the words (one in 2^`sample`, by
    /// their content) and from the offset of the last. (The pieces are of whole words.)
    pub fn references(
        reference: &[u8],
        of: usize,
        b: &[u8],
        piece: usize,
        most: usize,
        sample: u32,
    ) -> Vec<Vec<usize>> {
        let (per, per_ref) = ((piece / WORD).max(1), (of / WORD).max(1));
        let pieces = reference.len().div_ceil(per_ref * WORD);
        let mut votes = alloc::vec![alloc::vec![0usize; pieces]; b.len().div_ceil(per * WORD)];
        for c in anchors(reference, b, &Index::new(reference, sample)) {
            votes[c.b / per][(c.b as i64 + c.off) as usize / per_ref] += c.len;
        }
        votes
            .into_iter()
            .map(|v| {
                let mut order: Vec<usize> = (0..pieces).filter(|&j| v[j] > 0).collect();
                order.sort_by_key(|&j| (core::cmp::Reverse(v[j]), j));
                order.truncate(most);
                order.sort_unstable();
                order
            })
            .collect()
    }
}

#[cfg(feature = "encode")]
pub use encode::{encode, references};

#[cfg(all(test, feature = "encode", feature = "decode"))]
mod tests {
    use super::*;

    fn code(seed: u64, n: usize) -> Vec<u8> {
        let mut x = seed;
        (0..n)
            .flat_map(|_| {
                x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                // (words of few kinds, as code has)
                let w = ((x >> 33) as u32 % 64).wrapping_mul(0x0101_0407) ^ 0x9400_0000;
                w.to_be_bytes()
            })
            .collect()
    }

    #[test]
    fn round_trip() {
        let a = code(1, 20_000);
        // b: a with a word changed now and then, a run moved, some of its own, an odd tail
        let mut b = a.clone();
        for i in (0..b.len()).step_by(4 * 97) {
            b[i + 3] ^= 0x10;
        }
        let moved: Vec<u8> = b[4000..12000].to_vec();
        b.splice(40_000..40_000, moved);
        b.splice(20_000..20_000, code(2, 300));
        b.extend_from_slice(&[1, 2, 3]);
        let st = encode(&a, &b);
        assert!(st[EXTRA].len() < 3000, "{}", st[EXTRA].len());
        let back = decode(&a, [&st[0], &st[1], &st[2], &st[3]], b.len());
        assert_eq!(back.as_deref(), Some(&b[..]));
        // nothing alike, nothing at all
        for (a, b) in [(&a[..], &code(3, 500)[..]), (&[][..], &b[..100]), (&a[..], &[][..])] {
            let st = encode(a, b);
            assert_eq!(decode(a, [&st[0], &st[1], &st[2], &st[3]], b.len()).as_deref(), Some(b));
        }
    }

    #[test]
    fn bad_streams() {
        let a = code(1, 1000);
        let mut b = a.clone();
        b[100] ^= 1;
        let st = encode(&a, &b);
        let s = [&st[0][..], &st[1], &st[2], &st[3]];
        assert!(decode(&a, s, b.len() + 1).is_none());
        assert!(decode(&a[..2000], s, b.len()).is_none());
        let mut c = s;
        c[VALUES] = &[];
        assert!(decode(&a, c, b.len()).is_none());
        for k in (0..NUM_STREAMS).filter(|&k| !s[k].is_empty()) {
            let mut c = s;
            c[k] = &s[k][..s[k].len().saturating_sub(1)];
            assert!(decode(&a, c, b.len()).is_none(), "stream {k} cut");
        }
    }

    #[test]
    fn references_pick() {
        let a: Vec<u8> = [code(1, 4096), code(2, 4096), code(3, 4096)].concat();
        let b: Vec<u8> = [code(3, 4096), code(1, 4096)].concat();
        assert_eq!(references(&a, 4096 * 4, &b, 4096 * 4, 2, 0), [vec![2], vec![0]]);
        assert_eq!(references(&a, 4096 * 8, &b, 4096 * 4, 1, 0), [vec![1], vec![0]]);
    }
}
