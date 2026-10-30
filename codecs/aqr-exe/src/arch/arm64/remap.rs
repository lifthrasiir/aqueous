//! ARM64 code against another version of it (Courgette's idea): the targets of BL and ADRP that
//! the encoder maps to a reference image are written as the reference's, so that the words of
//! moved code match those of the reference where they did not. A flag per BL and per ADRP tells
//! which are, and a map of each value written so to its own target, as the shift from one to
//! the other, rebuilds them. The decoder needs no more than the streams: the map is over the
//! values that the flagged instructions hold, in order.
//!
//! The streams are those of [`super`] and four more (`exe-recons/arm64-remap`, FORMAT.md §M).

use alloc::vec::Vec;

use super::{low, NUM_STREAMS as NUM_BASE, STREAM_NAMES as BASE_NAMES};

/// Number of the streams: those of [`super`], then [`BLF`], [`ADRPF`], [`BLMAP`] and
/// [`ADRPMAP`].
pub const NUM_STREAMS: usize = NUM_BASE + 4;

/// The flags of BL, a bit each, the most significant first.
pub const BLF: usize = NUM_BASE;
/// The flags of ADRP.
pub const ADRPF: usize = NUM_BASE + 1;
/// The map of the BL targets flagged.
pub const BLMAP: usize = NUM_BASE + 2;
/// The map of the ADRP pages flagged.
pub const ADRPMAP: usize = NUM_BASE + 3;

/// Names of the streams, in order.
pub const STREAM_NAMES: [&str; NUM_STREAMS] = {
    let mut n = [""; NUM_STREAMS];
    let mut i = 0;
    while i < NUM_BASE {
        n[i] = BASE_NAMES[i];
        i += 1;
    }
    n[BLF] = "BLF";
    n[ADRPF] = "ADRPF";
    n[BLMAP] = "BLMAP";
    n[ADRPMAP] = "ADRPMAP";
    n
};

/// Bits of the field of a BL (`true`) or an ADRP.
const fn bits(bl: bool) -> u32 {
    if bl {
        26
    } else {
        21
    }
}

#[cfg(feature = "encode")]
mod encode {
    use super::super::{adrp_imm, encode_into, kind, Kind};
    use super::*;
    use alloc::vec;
    use core::array;

    /// The targets of BL (in words) and the pages of ADRP of an image, each as the field the
    /// filter writes (modulo 2^26 or 2^21), mapped to a reference image's.
    #[derive(Debug, Default)]
    pub struct Remap {
        /// Of BL, then of ADRP: the image's field and the reference's, by the image's.
        pub(crate) maps: [Vec<(u32, u32)>; 2],
        pub stats: Stats,
    }

    /// What [`Remap::new`] found.
    #[derive(Debug, Default, Clone, Copy)]
    pub struct Stats {
        pub functions: [usize; 2],
        pub matched: usize,
        /// Words of the reference's functions matched.
        pub matched_words: usize,
        pub words: [usize; 2],
        /// Entries of the maps of BL and ADRP.
        pub mapped: [usize; 2],
    }

    /// The words of an image's code sections, one after another.
    struct Words {
        w: Vec<u32>,
        pc: Vec<u64>,
        /// The first word of each section, and the end.
        starts: Vec<usize>,
    }

    impl Words {
        fn new(sections: &[(u64, &[u8])]) -> Words {
            let (mut w, mut pc, mut starts) = (Vec::new(), Vec::new(), Vec::new());
            for &(va, code) in sections {
                if !va.is_multiple_of(4) {
                    continue;
                }
                starts.push(w.len());
                for (k, x) in code.as_chunks::<4>().0.iter().enumerate() {
                    w.push(u32::from_le_bytes(*x));
                    pc.push(va.wrapping_add(4 * k as u64));
                }
            }
            starts.push(w.len());
            Words { w, pc, starts }
        }

        /// The absolute field of word `i` of `k`, BL or ADRP, as the filter makes it.
        fn field(&self, i: usize, k: Kind) -> u32 {
            let (w, pc) = (self.w[i], self.pc[i]);
            match k {
                Kind::Bl => w.wrapping_add((pc >> 2) as u32) & low(26),
                _ => adrp_imm(w).wrapping_add((pc >> 12) as u32) & low(21),
            }
        }

        /// The functions, from each BL target within the code to the next (and from the
        /// start of each section), as ranges of words with the hash of their words, BL and
        /// ADRP fields cleared.
        fn functions(&self) -> Vec<(u64, usize, usize)> {
            // the sections by address, of the words from their first
            let mut secs: Vec<(u64, usize, usize)> =
                self.starts.windows(2).filter(|s| s[0] < s[1]).map(|s| (self.pc[s[0]] >> 2, s[0], s[1])).collect();
            secs.sort_unstable();
            let mut cuts: Vec<usize> = self.starts.clone();
            for i in 0..self.w.len() {
                if kind(self.w[i]) != Kind::Bl {
                    continue;
                }
                let s = ((self.w[i] << 6) as i32 >> 6) as i64 as u64;
                let t = (self.pc[i] >> 2).wrapping_add(s);
                let j = secs.partition_point(|x| x.0 <= t);
                if let Some(&(at, from, to)) = j.checked_sub(1).map(|j| &secs[j]) {
                    if t - at < (to - from) as u64 {
                        cuts.push(from + (t - at) as usize);
                    }
                }
            }
            cuts.sort_unstable();
            cuts.dedup();
            cuts.windows(2)
                .map(|c| {
                    let mut h = 0xcbf2_9ce4_8422_2325u64 ^ (c[1] - c[0]) as u64;
                    for &w in &self.w[c[0]..c[1]] {
                        h = (h.rotate_left(23) ^ norm(w) as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15);
                    }
                    (h, c[0], c[1])
                })
                .collect()
        }
    }

    /// The entries of `f` (by hash first) whose hash no other has.
    fn unique(mut f: Vec<(u64, usize)>) -> Vec<(u64, usize)> {
        f.sort_unstable();
        let mut out = Vec::with_capacity(f.len());
        let mut i = 0;
        while i < f.len() {
            let j = i + f[i..].iter().take_while(|x| x.0 == f[i].0).count();
            if j == i + 1 {
                out.push(f[i]);
            }
            i = j;
        }
        out
    }

    /// Of `votes` (each a value of the reference and one of the image), the image's value
    /// most voted for each of the reference's, by the reference's.
    fn majority(votes: &mut [(u32, u32)]) -> Vec<(u32, u32)> {
        votes.sort_unstable();
        let mut fwd: Vec<(u32, u32)> = Vec::new();
        let mut i = 0;
        while i < votes.len() {
            let a = votes[i].0;
            let (mut best, mut n) = ((0, 0), 0);
            while i < votes.len() && votes[i].0 == a {
                let j = i + votes[i..].iter().take_while(|v| **v == votes[i]).count();
                if j - i > n {
                    (best, n) = (votes[i], j - i);
                }
                i = j;
            }
            fwd.push(best);
        }
        fwd
    }

    /// Of `fwd` (the reference's values to the image's), the image's values that one value of
    /// the reference maps to, with it, by the image's.
    fn inverse(fwd: &[(u32, u32)]) -> Vec<(u32, u32)> {
        let mut inv: Vec<(u32, u32)> = fwd.iter().map(|&(a, b)| (b, a)).collect();
        inv.sort_unstable();
        let mut out = Vec::with_capacity(inv.len());
        let mut i = 0;
        while i < inv.len() {
            let j = i + inv[i..].iter().take_while(|x| x.0 == inv[i].0).count();
            if j == i + 1 {
                out.push(inv[i]);
            }
            i = j;
        }
        out
    }

    /// `w` with its BL or ADRP field cleared.
    fn norm(w: u32) -> u32 {
        match kind(w) {
            Kind::Bl => w & !low(26),
            Kind::Adrp => w & 0x9f00_001f,
            _ => w,
        }
    }

    /// Rounds of matching after the first, at most.
    const ROUNDS: usize = 16;

    impl Remap {
        /// The map of the image of code sections `image` (each its address and its bytes) to
        /// the reference image `reference`. Functions whose words hash alike and uniquely in
        /// both, of one length, are matched, and the targets and pages at the same places in
        /// them vote. Then, in rounds, a function not matched yet is matched to the one that
        /// its start maps to by the votes so far, or else to the one its matched neighbours'
        /// shift tells; their words from the start and from the end, alike as far as they go,
        /// vote. (Asking those to be some share of the words, or aligning the words between
        /// them, maps fewer.)
        pub fn new(reference: &[(u64, &[u8])], image: &[(u64, &[u8])]) -> Remap {
            let (a, b) = (Words::new(reference), Words::new(image));
            let (fa, fb) = (a.functions(), b.functions());
            let mut stats =
                Stats { functions: [fa.len(), fb.len()], words: [a.w.len(), b.w.len()], ..Stats::default() };
            let start = |w: &Words, i: usize| (w.pc[i] >> 2) as u32 & low(26);
            let mut votes: [Vec<(u32, u32)>; 2] = [Vec::new(), Vec::new()];
            let vote = |votes: &mut [Vec<(u32, u32)>; 2], i: usize, k: usize| match kind(a.w[i]) {
                Kind::Bl if kind(b.w[k]) == Kind::Bl => votes[0].push((a.field(i, Kind::Bl), b.field(k, Kind::Bl))),
                Kind::Adrp if kind(b.w[k]) == Kind::Adrp => {
                    votes[1].push((a.field(i, Kind::Adrp), b.field(k, Kind::Adrp)))
                }
                _ => {}
            };
            // of each function of the reference, the one of the image matched
            let mut pair: Vec<Option<usize>> = vec![None; fa.len()];
            let mut taken = vec![false; fb.len()];
            let (ua, ub) = (
                unique(fa.iter().enumerate().map(|(i, f)| (f.0, i)).collect()),
                unique(fb.iter().enumerate().map(|(i, f)| (f.0, i)).collect()),
            );
            let mut j = 0;
            for &(h, i) in &ua {
                while j < ub.len() && ub[j].0 < h {
                    j += 1;
                }
                let Some(&(_, k)) = ub.get(j).filter(|x| x.0 == h && fb[x.1].2 - fb[x.1].1 == fa[i].2 - fa[i].1) else {
                    continue;
                };
                (pair[i], taken[k]) = (Some(k), true);
                let (a0, a1, b0) = (fa[i].1, fa[i].2, fb[k].1);
                votes[0].push((start(&a, a0), start(&b, b0)));
                for (i, k) in (a0..a1).zip(b0..) {
                    vote(&mut votes, i, k);
                }
            }
            // the functions of the image by their starts
            let mut by_start: Vec<(u32, usize)> = fb.iter().enumerate().map(|(k, f)| (start(&b, f.1), k)).collect();
            by_start.sort_unstable();
            let at = |s: u32| by_start.binary_search_by_key(&s, |x| x.0).ok().map(|j| by_start[j].1);
            for _ in 0..ROUNDS {
                let fwd = majority(&mut votes[0]);
                let shift = |i: usize| pair[i].map(|k| start(&b, fb[k].1).wrapping_sub(start(&a, fa[i].1)) & low(26));
                // the shift of the last function matched before each, and of the next after
                let mut before = vec![None; fa.len()];
                let mut after = vec![None; fa.len()];
                for i in 1..fa.len() {
                    before[i] = shift(i - 1).or(before[i - 1]);
                }
                for i in (0..fa.len().saturating_sub(1)).rev() {
                    after[i] = shift(i + 1).or(after[i + 1]);
                }
                let mut new = 0;
                for i in 0..fa.len() {
                    if pair[i].is_some() {
                        continue;
                    }
                    let (a0, a1) = (fa[i].1, fa[i].2);
                    let s = start(&a, a0);
                    let voted = fwd.binary_search_by_key(&s, |x| x.0).ok().map(|j| fwd[j].1);
                    let near = before[i].filter(|_| before[i] == after[i]).map(|d| s.wrapping_add(d) & low(26));
                    let Some(k) = voted.or(near).and_then(at).filter(|&k| !taken[k]) else { continue };
                    let (b0, b1) = (fb[k].1, fb[k].2);
                    let same = |(i, k): (usize, usize)| norm(a.w[i]) == norm(b.w[k]);
                    let head = (a0..a1).zip(b0..b1).take_while(|&p| same(p)).count();
                    let tail = (a0 + head..a1).rev().zip((b0 + head..b1).rev()).take_while(|&p| same(p)).count();
                    (pair[i], taken[k]) = (Some(k), true);
                    new += 1;
                    votes[0].push((s, start(&b, b0)));
                    for (i, k) in (a0..a0 + head).zip(b0..) {
                        vote(&mut votes, i, k);
                    }
                    for (i, k) in (a1 - tail..a1).zip(b1 - tail..) {
                        vote(&mut votes, i, k);
                    }
                }
                if new == 0 {
                    break;
                }
            }
            for (i, p) in pair.iter().enumerate() {
                if p.is_some() {
                    stats.matched += 1;
                    stats.matched_words += fa[i].2 - fa[i].1;
                }
            }
            // (values mapped to themselves too: their flags then go with their neighbours', which
            // costs less than the maps' entries saved)
            let maps = votes.map(|mut v| inverse(&majority(&mut v)));
            stats.mapped = [maps[0].len(), maps[1].len()];
            Remap { maps, stats }
        }

        /// This map composed with `parent`'s, the map of the reference to an image of its own:
        /// to that image's values, which the reference is written in. Values that two of the
        /// image's would map to are left unmapped.
        pub fn compose(self, parent: &Remap) -> Remap {
            let maps = [0, 1].map(|i| {
                let mut m: Vec<(u32, u32)> =
                    self.maps[i].iter().map(|&(b, a)| (b, parent.get(i == 0, a).unwrap_or(a))).collect();
                m.sort_unstable_by_key(|x| x.1);
                let mut keep = vec![true; m.len()];
                for j in 1..m.len() {
                    if m[j].1 == m[j - 1].1 {
                        (keep[j - 1], keep[j]) = (false, false);
                    }
                }
                let mut m: Vec<_> = m.into_iter().zip(keep).filter(|x| x.1).map(|x| x.0).collect();
                m.sort_unstable();
                m
            });
            let mut stats = self.stats;
            stats.mapped = [maps[0].len(), maps[1].len()];
            Remap { maps, stats }
        }

        /// Whether every value is mapped to itself, if any.
        pub fn is_identity(&self) -> bool {
            self.maps.iter().flatten().all(|x| x.0 == x.1)
        }

        /// The reference's value for `f`, the field of a BL (`bl`) or an ADRP of the image.
        pub fn get(&self, bl: bool, f: u32) -> Option<u32> {
            let m = &self.maps[!bl as usize];
            m.binary_search_by_key(&f, |x| x.0).ok().map(|i| m[i].1)
        }
    }

    /// A sample of the functions of an image ([`Remap::new`]'s), to tell images alike: the
    /// hashes of those that hash to a multiple of [`SAMPLE`].
    #[derive(Debug, Clone)]
    pub struct Signature(Vec<u64>);

    /// What hashes of [`Signature`]s are multiples of.
    pub const SAMPLE: u64 = 16;

    impl Signature {
        pub fn of(image: &[(u64, &[u8])]) -> Signature {
            let mut h: Vec<u64> =
                Words::new(image).functions().into_iter().map(|f| f.0).filter(|h| h.is_multiple_of(SAMPLE)).collect();
            h.sort_unstable();
            h.dedup();
            Signature(h)
        }

        /// The share of the hashes of the larger of the two in both.
        pub fn alike(&self, other: &Signature) -> f64 {
            let (a, b) = (&self.0, &other.0);
            let (mut i, mut j, mut n) = (0, 0, 0);
            while i < a.len() && j < b.len() {
                match a[i].cmp(&b[j]) {
                    core::cmp::Ordering::Less => i += 1,
                    core::cmp::Ordering::Greater => j += 1,
                    core::cmp::Ordering::Equal => (i, j, n) = (i + 1, j + 1, n + 1),
                }
            }
            n as f64 / a.len().max(b.len()).max(1) as f64
        }

        pub fn len(&self) -> usize {
            self.0.len()
        }

        pub fn is_empty(&self) -> bool {
            self.0.is_empty()
        }
    }

    /// The streams of a section, as [`encode`] gives them.
    #[derive(Debug)]
    pub struct EncodedStreams {
        streams: [Vec<u8>; NUM_STREAMS],
        n: [usize; 2],
        flagged: [usize; 2],
    }

    impl EncodedStreams {
        pub fn streams(&self) -> &[Vec<u8>; NUM_STREAMS] {
            &self.streams
        }

        /// Instructions BL and ADRP, and those of them flagged.
        pub fn flagged(&self) -> [(usize, usize); 2] {
            [0, 1].map(|i| (self.n[i], self.flagged[i]))
        }
    }

    /// Bits, the most significant of each byte first.
    #[derive(Default)]
    struct Bits {
        out: Vec<u8>,
        n: usize,
    }

    impl Bits {
        fn push(&mut self, b: bool) {
            if self.n.is_multiple_of(8) {
                self.out.push(0);
            }
            *self.out.last_mut().unwrap() |= (b as u8) << (7 - self.n % 8);
            self.n += 1;
        }
    }

    /// The streams of the code section `code` whose first byte is at address `origin`, its
    /// BL and ADRP targets written as `remap` maps them.
    pub fn encode(code: &[u8], origin: u64, remap: &Remap) -> EncodedStreams {
        let mut base: [Vec<u8>; NUM_BASE] = array::from_fn(|_| Vec::new());
        let mut flags: [Bits; 2] = Default::default();
        let mut pairs: [Vec<(u32, u32)>; 2] = [Vec::new(), Vec::new()];
        encode_into(&mut base, code, origin, |bl, f| {
            let i = !bl as usize;
            let r = remap.get(bl, f);
            flags[i].push(r.is_some());
            let Some(r) = r else { return f };
            pairs[i].push((r, f));
            r
        });
        let [bf, af] = flags;
        let (n, flagged) = ([bf.n, af.n], [pairs[0].len(), pairs[1].len()]);
        let maps = [0, 1].map(|i| {
            let p = &mut pairs[i];
            p.sort_unstable();
            p.dedup();
            debug_assert!(p.windows(2).all(|w| w[0].0 != w[1].0), "a value mapped twice");
            let b = bits(i == 0);
            let (mut out, mut last) = (Vec::new(), 0u32);
            for &(v, t) in p.iter() {
                let s = t.wrapping_sub(v) & low(b);
                let d = s.wrapping_sub(last) & low(b);
                last = s;
                // the difference sign-extended from its bits, zigzag coded, as a LEB128
                let d = ((d << (32 - b)) as i32) >> (32 - b);
                let mut z = ((d << 1) ^ (d >> 31)) as u32;
                while z >= 0x80 {
                    out.push(z as u8 | 0x80);
                    z >>= 7;
                }
                out.push(z as u8);
            }
            out
        });
        let [bm, am] = maps;
        let mut streams = base.into_iter().chain([bf.out, af.out, bm, am]);
        EncodedStreams { streams: array::from_fn(|_| streams.next().unwrap()), n, flagged }
    }
}

#[cfg(feature = "encode")]
pub use encode::*;

#[cfg(feature = "decode")]
mod decode {
    use super::super::{decode_from, kind, take, Kind};
    use super::*;

    /// Flags, the most significant bit of each byte first.
    struct Flags<'a> {
        s: &'a [u8],
        n: usize,
    }

    impl Flags<'_> {
        fn next(&mut self) -> Option<bool> {
            let b = *self.s.get(self.n / 8)? >> (7 - self.n % 8) & 1;
            self.n += 1;
            Some(b != 0)
        }

        /// Whether all were read, the bits left in the last byte 0.
        fn done(&self) -> bool {
            let pad = if self.n.is_multiple_of(8) { 0 } else { 0xff >> (self.n % 8) };
            self.s.len() == self.n.div_ceil(8) && self.s.last().is_none_or(|&b| b & pad == 0)
        }
    }

    /// The map of the values in `vals`, sorted and deduplicated, of `bits` bits from
    /// stream `s`: each value and its target.
    fn map(mut vals: Vec<u32>, mut s: &[u8], b: u32) -> Option<Vec<(u32, u32)>> {
        vals.sort_unstable();
        vals.dedup();
        let (mut out, mut last) = (Vec::with_capacity(vals.len()), 0u32);
        for v in vals {
            let mut z = 0u32;
            for sh in (0..35).step_by(7) {
                let x = take(&mut s, 1)?[0];
                if sh == 28 && x >> 4 != 0 {
                    return None;
                }
                z |= ((x & 0x7f) as u32) << sh;
                if x < 0x80 {
                    break;
                }
            }
            if z >> b != 0 {
                return None;
            }
            let d = (z >> 1) ^ (z & 1).wrapping_neg();
            last = last.wrapping_add(d) & low(b);
            out.push((v, v.wrapping_add(last) & low(b)));
        }
        s.is_empty().then_some(out)
    }

    /// The section of `len` bytes at address `origin` from its `streams`; `None` where they
    /// do not make one, or are not all read.
    pub fn decode(streams: [&[u8]; NUM_STREAMS], origin: u64, len: usize) -> Option<Vec<u8>> {
        // the values flagged, of the words in CODE
        let code = streams[0].get(..len / 4 * 4)?;
        let mut flags = [Flags { s: streams[BLF], n: 0 }, Flags { s: streams[ADRPF], n: 0 }];
        // (as many as the flags set, at most: no more memory than that)
        let ones = |s: &[u8]| s.iter().map(|b| b.count_ones() as usize).sum::<usize>();
        let mut vals: [Vec<u32>; 2] = [streams[BLF], streams[ADRPF]].map(|s| Vec::with_capacity(ones(s)));
        for w in code.as_chunks::<4>().0 {
            let w = u32::from_be_bytes(*w);
            let (i, f) = match kind(w) {
                Kind::Bl => (0, w & low(26)),
                Kind::Adrp => (1, super::super::adrp_imm(w)),
                _ => continue,
            };
            if flags[i].next()? {
                vals[i].push(f);
            }
        }
        if !flags.iter().all(Flags::done) {
            return None;
        }
        let [bv, av] = vals;
        let maps = [map(bv, streams[BLMAP], bits(true))?, map(av, streams[ADRPMAP], bits(false))?];
        let mut flags = [Flags { s: streams[BLF], n: 0 }, Flags { s: streams[ADRPF], n: 0 }];
        let mut s: [&[u8]; NUM_BASE] = core::array::from_fn(|i| streams[i]);
        let out = decode_from(&mut s, origin, len, |bl, f| {
            let i = !bl as usize;
            if !flags[i].next()? {
                return Some(f);
            }
            let m = &maps[i];
            Some(m[m.binary_search_by_key(&f, |x| x.0).ok()?].1)
        })?;
        s.iter().all(|s| s.is_empty()).then_some(out)
    }
}

#[cfg(feature = "decode")]
pub use decode::decode;

#[cfg(all(test, feature = "encode", feature = "decode"))]
mod tests {
    use super::*;
    use alloc::vec;
    use core::array;

    fn roundtrip(code: &[u8], origin: u64, remap: &Remap) -> EncodedStreams {
        let e = encode(code, origin, remap);
        let s: [&[u8]; NUM_STREAMS] = array::from_fn(|i| &e.streams()[i][..]);
        assert_eq!(decode(s, origin, code.len()).as_deref(), Some(code));
        e
    }

    fn code(words: &[u32]) -> Vec<u8> {
        words.iter().flat_map(|w| w.to_le_bytes()).collect()
    }

    fn bl(from: usize, to: usize) -> u32 {
        0x9400_0000 | (to.wrapping_sub(from) as u32 & low(26))
    }

    /// Functions of distinct bodies calling each other, and an ADRP to a page of their own.
    fn image(order: &[usize], gap: usize) -> Vec<u32> {
        let len = |f: usize| 3 + f % 4;
        let mut at = vec![0; order.len()];
        let mut k = 0;
        for &f in order {
            at[f] = k;
            k += len(f) + if f == order[0] { gap } else { 0 };
        }
        let mut w = Vec::new();
        for &f in order {
            let callee = (f * 7 + 3) % order.len();
            w.push(0xa9bf_7bfd);
            w.push(bl(w.len(), at[callee]));
            // adrp x(f % 31), page f
            let page = (f as u32 + 16).wrapping_sub((w.len() >> 10) as u32);
            w.push(0x9000_0000 | (f as u32 % 31) | (page & 3) << 29 | (page >> 2 & 0x7_ffff) << 5);
            w.extend((3..len(f)).map(|j| 0xd280_0000 | (f as u32) << 5 | j as u32));
            if f == order[0] {
                w.extend((0..gap).map(|_| 0xd503_201f));
            }
        }
        w
    }

    #[test]
    fn moved_functions() {
        let order: Vec<usize> = (0..40).collect();
        let a = code(&image(&order, 0));
        // the same functions, moved by some inserted words and in another order
        let mut order_b = order.clone();
        order_b.swap(5, 30);
        let b = code(&image(&order_b, 9));
        let r = Remap::new(&[(0x4000, &a)], &[(0x8000, &b)]);
        assert!(r.stats.matched > 30, "{:?}", r.stats);
        let e = roundtrip(&b, 0x8000, &r);
        let [(n, k), _] = e.flagged();
        assert!(k * 10 > n * 8, "{n} {k}");
        // the words of B (but the padding) are those of A, where as they are the calls are not
        let words = |c: &[u8]| -> Vec<u32> { c.chunks(4).map(|w| u32::from_be_bytes(w.try_into().unwrap())).collect() };
        let mut wa = words(&super::super::encode(&a, 0x4000).streams()[0]);
        wa.sort_unstable();
        let found = |c: &[u8]| words(c).iter().filter(|w| wa.binary_search(w).is_ok()).count();
        let plain = super::super::encode(&b, 0x8000);
        let (before, after) = (found(&plain.streams()[0]), found(&e.streams()[0]));
        // (the first function, padded, matched by its words from the start)
        assert_eq!(after, b.len() / 4 - 9, "{before} {after}");
        assert!(before + 60 < after, "{before} {after}");
    }

    /// C against B against A: C written in A's address space, as B is.
    #[test]
    fn composed() {
        let order: Vec<usize> = (0..40).collect();
        let (a, b, c) = (code(&image(&order, 0)), code(&image(&order, 5)), code(&image(&order, 11)));
        let (sa, sb, sc) = ([(0x4000, &a[..])], [(0x4000, &b[..])], [(0x4000, &c[..])]);
        let rb = Remap::new(&sa, &sb);
        let rc = Remap::new(&sb, &sc).compose(&rb);
        let (eb, ec) = (roundtrip(&b, 0x4000, &rb), roundtrip(&c, 0x4000, &rc));
        // past the first function and its padding, C's words are B's
        let tail = |e: &EncodedStreams, pad: usize| e.streams()[0][4 * (3 + pad)..].to_vec();
        assert_eq!(tail(&eb, 5), tail(&ec, 11));
        assert!(Signature::of(&sa).alike(&Signature::of(&sc)) > 0.0 || Signature::of(&sa).is_empty());
    }

    #[test]
    fn no_map_and_corrupt_streams() {
        let words = [0x9400_0010, 0x9000_0003, 0x5400_0041, 0x1234_5678];
        let c = code(&words);
        let e = roundtrip(&c, 0x1000, &Remap::default());
        assert_eq!(e.streams()[BLF], [0]);
        assert_eq!(e.streams()[ADRPF], [0]);
        // a flag set with no map; padding bits set
        let s: [&[u8]; NUM_STREAMS] = array::from_fn(|i| &e.streams()[i][..]);
        for (i, bad) in [(BLF, [0x80]), (ADRPF, [0x01])] {
            let mut t = s;
            t[i] = &bad;
            assert_eq!(decode(t, 0x1000, c.len()), None);
        }
        // a map with an entry too many
        let mut t = s;
        t[BLMAP] = &[0];
        assert_eq!(decode(t, 0x1000, c.len()), None);
    }

    /// A map built by hand, each value of the image mapped; any targets round-trip.
    #[test]
    fn any_targets() {
        let mut x = 0x1234_5678u32;
        let mut words = Vec::new();
        for _ in 0..5000 {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            words.push(if x & 1 == 0 { 0x9400_0000 | x >> 6 } else { 0x9000_0000 | (x & 0x60ff_ffe0) });
        }
        let c = code(&words);
        let plain = super::super::encode(&c, 0);
        let mut maps: [Vec<(u32, u32)>; 2] = [Vec::new(), Vec::new()];
        for w in plain.streams()[0].chunks(4) {
            let w = u32::from_be_bytes(w.try_into().unwrap());
            let (i, f) = if w >> 26 == 0x25 { (0, w & low(26)) } else { (1, super::super::adrp_imm(w)) };
            if f % 3 != 0 {
                maps[i].push((f, f.wrapping_mul(2654435761) & low(bits(i == 0))));
            }
        }
        for m in &mut maps {
            m.sort_unstable();
            m.dedup_by_key(|x| x.0);
        }
        let r = Remap { maps, stats: Stats::default() };
        roundtrip(&c, 0, &r);
        roundtrip(&c, 0xffff_ffff_ffff_f000, &r);
    }
}
