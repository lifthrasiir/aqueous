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
    use core::array;

    /// The targets of BL (in words) and the pages of ADRP of an image, each as the field the
    /// filter writes (modulo 2^26 or 2^21), mapped to a reference image's.
    #[derive(Debug, Default)]
    pub struct Remap {
        /// Of BL, then of ADRP: the image's field and the reference's, by the image's.
        pub(crate) maps: [Vec<(u32, u32)>; 2],
        pub stats: Stats,
    }

    use crate::arch::matching::{self, Code};
    pub use crate::arch::matching::{Signature, Stats, SAMPLE};

    /// The code of an image's code sections, one after another, by word ([`matching`]): BL
    /// targets (space 0) and ADRP pages (space 1) as the filter writes them.
    fn code(sections: &[(u64, &[u8])]) -> Code<u32> {
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
        // the sections by address, of the words from their first
        let mut secs: Vec<(u64, usize, usize)> =
            starts.windows(2).filter(|s| s[0] < s[1]).map(|s| (pc[s[0]] >> 2, s[0], s[1])).collect();
        secs.sort_unstable();
        let (mut cuts, mut refs) = (starts, Vec::new());
        for (i, (&w, &pc)) in w.iter().zip(&pc).enumerate() {
            match kind(w) {
                Kind::Bl => {
                    refs.push((i, 0, (w.wrapping_add((pc >> 2) as u32) & low(26)) as u64));
                    // (a function from where it goes)
                    let t = (pc >> 2).wrapping_add(((w << 6) as i32 >> 6) as i64 as u64);
                    let j = secs.partition_point(|x| x.0 <= t);
                    if let Some(&(at, from, to)) = j.checked_sub(1).map(|j| &secs[j]) {
                        if t - at < (to - from) as u64 {
                            cuts.push(from + (t - at) as usize);
                        }
                    }
                }
                Kind::Adrp => refs.push((i, 1, (adrp_imm(w).wrapping_add((pc >> 12) as u32) & low(21)) as u64)),
                _ => {}
            }
        }
        cuts.sort_unstable();
        cuts.dedup();
        let starts = cuts[..cuts.len() - 1].iter().map(|&c| (pc[c] >> 2) & low(26) as u64).collect();
        Code::new(w.iter().map(|&w| norm(w)).collect(), cuts, starts, refs)
    }

    /// `w` with its BL or ADRP field cleared.
    fn norm(w: u32) -> u32 {
        match kind(w) {
            Kind::Bl => w & !low(26),
            Kind::Adrp => w & 0x9f00_001f,
            _ => w,
        }
    }

    fn of_u64(maps: Vec<Vec<(u64, u64)>>) -> [Vec<(u32, u32)>; 2] {
        let mut m = maps.into_iter().map(|m| m.into_iter().map(|(b, a)| (b as u32, a as u32)).collect());
        array::from_fn(|_| m.next().unwrap_or_default())
    }

    impl Remap {
        /// The map of the image of code sections `image` (each its address and its bytes) to
        /// the reference image `reference`, of their functions matched ([`matching::map`]).
        pub fn new(reference: &[(u64, &[u8])], image: &[(u64, &[u8])]) -> Remap {
            let (maps, stats) = matching::map(&code(reference), &code(image), 2, low(26) as u64);
            Remap { maps: of_u64(maps), stats }
        }

        /// This map composed with `parent`'s, the map of the reference to an image of its own:
        /// to that image's values, which the reference is written in. Values that two of the
        /// image's would map to are left unmapped.
        pub fn compose(self, parent: &Remap) -> Remap {
            let maps = self.maps.map(|m| m.into_iter().map(|(b, a)| (b as u64, a as u64)).collect());
            let maps = of_u64(matching::compose(&maps, |i, a| parent.get(i == 0, a as u32).map(u64::from)));
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

    /// The [`Signature`] of the image of code sections `image`.
    pub fn signature(image: &[(u64, &[u8])]) -> Signature {
        Signature::of(&code(image))
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
        assert!(signature(&sa).alike(&signature(&sc)) > 0.0 || signature(&sa).is_empty());
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
