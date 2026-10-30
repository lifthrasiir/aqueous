//! x86 code against another version of it, as `arm64::remap`: the references that the
//! encoder maps to a reference image (the targets of `CALL rel32` and of other jumps of 32-bit
//! offsets, and `[rip+disp32]`, or `[disp32]` in x86-32) are written as the reference's, so
//! that the streams of moved code match those of the reference. A flag per reference of each
//! kind tells which, and a map of each value written so to its own rebuilds them; the map is
//! over the values that the flagged references hold, so the decoder needs no more than the
//! streams. It notes where each flagged reference is as it decodes, and adds its shift there
//! once the map is known.
//!
//! The streams are those of [`super`] and four more (`exe-recons/x86-remap`, FORMAT.md §N).

use alloc::vec::Vec;

use super::{Ref, NUM_STREAMS as NUM_BASE, STREAM_NAMES as BASE_NAMES};

/// Number of the streams: those of [`super`], then [`CALLF`], [`JUMPF`], [`ADDRF`] and [`MAP`].
pub const NUM_STREAMS: usize = NUM_BASE + 4;

/// The flags of the targets of `CALL rel32`, a bit each, the most significant first.
pub const CALLF: usize = NUM_BASE;
/// The flags of the targets of other jumps of 32-bit offsets.
pub const JUMPF: usize = NUM_BASE + 1;
/// The flags of `[rip+disp32]` (`[disp32]` in x86-32).
pub const ADDRF: usize = NUM_BASE + 2;
/// The map of the values flagged.
pub const MAP: usize = NUM_BASE + 3;

/// Names of the streams, in order.
pub const STREAM_NAMES: [&str; NUM_STREAMS] = {
    let mut n = [""; NUM_STREAMS];
    let mut i = 0;
    while i < NUM_BASE {
        n[i] = BASE_NAMES[i];
        i += 1;
    }
    n[CALLF] = "ST_CALLF";
    n[JUMPF] = "ST_JUMPF";
    n[ADDRF] = "ST_ADDRF";
    n[MAP] = "ST_MAP";
    n
};

/// The flag stream of references of `r`.
const fn flags_of(r: Ref) -> usize {
    match r {
        Ref::Call => CALLF,
        Ref::Jump => JUMPF,
        Ref::Address => ADDRF,
    }
}

/// The value of a reference as the streams hold it: 64 bits in x86-64, 32 in x86-32.
const fn width(is64: bool) -> u64 {
    if is64 {
        u64::MAX
    } else {
        u32::MAX as u64
    }
}

#[cfg(feature = "encode")]
mod encode {
    use super::super::{encode_with, EncodedStreams as Base};
    use super::*;
    use crate::arch::matching::{self, Code};
    pub use crate::arch::matching::{Signature, Stats, SAMPLE};

    /// The references of an image (their values, of the width of its code) mapped to a
    /// reference image's.
    #[derive(Debug, Default)]
    pub struct Remap {
        /// The image's values and the reference's, by the image's.
        pub(crate) map: Vec<(u64, u64)>,
        pub stats: Stats,
    }

    /// The code of an image's code sections, one after another, by byte ([`matching`]): the
    /// references of [`Ref`], all of space 0, their fields cleared.
    fn code(sections: &[(u64, &[u8])], is64: bool) -> Code<u8> {
        let (mut units, mut cuts, mut refs, mut calls, mut secs) =
            (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
        for &(va, c) in sections {
            let base = units.len();
            cuts.push(base);
            secs.push((va, base, c.len()));
            units.extend_from_slice(c);
            let mut hook = |r: Ref, at: u64, v: u64| {
                let v = v & width(is64);
                // (the fields are within the section: the encoder reads its last bytes from a
                // copy padded with zeros, which no reference's field reaches)
                if let Some(u) = at.checked_sub(va).filter(|&u| u + 4 <= c.len() as u64) {
                    refs.push((base + u as usize, 0u8, v));
                    if r == Ref::Call {
                        calls.push(v);
                    }
                }
                v
            };
            if encode_with(c, va, is64, &mut hook).is_none() {
                continue;
            }
        }
        cuts.push(units.len());
        refs.sort_unstable_by_key(|r| r.0);
        refs.dedup_by_key(|r| r.0);
        for &(u, _, _) in &refs {
            units[u..u + 4].fill(0);
        }
        // (a function from where calls go)
        secs.sort_unstable();
        for t in calls {
            let j = secs.partition_point(|s| s.0 <= t);
            if let Some(&(va, base, len)) = j.checked_sub(1).map(|j| &secs[j]) {
                if t - va < len as u64 {
                    cuts.push(base + (t - va) as usize);
                }
            }
        }
        cuts.sort_unstable();
        cuts.dedup();
        // the address of each function's start
        let starts = cuts[..cuts.len() - 1]
            .iter()
            .map(|&c| {
                let j = secs.iter().position(|s| s.1 <= c && c < s.1 + s.2.max(1)).unwrap_or(0);
                secs[j].0.wrapping_add((c - secs[j].1) as u64) & width(is64)
            })
            .collect();
        Code::new(units, cuts, starts, refs)
    }

    /// An image's code as [`Remap::of`] matches it, parsed once (of about the code's size and
    /// 16 bytes a reference).
    pub struct Image {
        code: Code<u8>,
        is64: bool,
    }

    impl Image {
        /// Of the code sections `sections` (each its address and its bytes), of x86-64 or x86-32.
        pub fn new(sections: &[(u64, &[u8])], is64: bool) -> Image {
            Image { code: code(sections, is64), is64 }
        }

        pub fn signature(&self) -> Signature {
            Signature::of(&self.code)
        }
    }

    impl Remap {
        /// The map of the image of code sections `image` (each its address and its bytes) to
        /// the reference image `reference`, of their functions matched ([`matching::map`]).
        pub fn new(reference: &[(u64, &[u8])], image: &[(u64, &[u8])], is64: bool) -> Remap {
            Remap::of(&Image::new(reference, is64), &Image::new(image, is64))
        }

        /// [`Remap::new`] of images parsed.
        pub fn of(reference: &Image, image: &Image) -> Remap {
            let w = width(image.is64);
            let (mut maps, stats) = matching::map(&reference.code, &image.code, 1, w);
            Remap { map: maps.pop().unwrap_or_default(), stats }
        }

        /// This map composed with `parent`'s, the map of the reference to an image of its own:
        /// to that image's values, which the reference is written in. Values that two of the
        /// image's would map to are left unmapped.
        pub fn compose(self, parent: &Remap) -> Remap {
            let mut maps = matching::compose(&[self.map], |_, a| parent.get(a));
            let map = maps.pop().unwrap_or_default();
            let mut stats = self.stats;
            stats.mapped = [map.len(), 0];
            Remap { map, stats }
        }

        /// Whether every value is mapped to itself, if any.
        pub fn is_identity(&self) -> bool {
            self.map.iter().all(|x| x.0 == x.1)
        }

        /// The reference's value for `v`, an image's.
        pub fn get(&self, v: u64) -> Option<u64> {
            self.map.binary_search_by_key(&v, |x| x.0).ok().map(|i| self.map[i].1)
        }
    }

    /// The [`Signature`] of the image of code sections `image`.
    pub fn signature(image: &[(u64, &[u8])], is64: bool) -> Signature {
        Image::new(image, is64).signature()
    }

    /// The streams of a section, as [`encode`] gives them.
    #[derive(Debug)]
    pub struct EncodedStreams {
        pub base: Base,
        streams: [Vec<u8>; 4],
        n: [usize; 3],
        flagged: [usize; 3],
    }

    impl EncodedStreams {
        /// The streams from [`CALLF`] on.
        pub fn extra(&self) -> &[Vec<u8>; 4] {
            &self.streams
        }

        /// References of each kind (`CALL`, other jumps, addresses), and those of them flagged.
        pub fn flagged(&self) -> [(usize, usize); 3] {
            [0, 1, 2].map(|i| (self.n[i], self.flagged[i]))
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

    /// The streams of the code section `code` at address `origin`, of x86-64 or x86-32, its
    /// references written as `remap` maps them.
    pub fn encode(code: &[u8], origin: u64, is64: bool, remap: &Remap) -> Option<EncodedStreams> {
        let mut flags: [Bits; 3] = Default::default();
        let mut pairs: Vec<(u64, u64)> = Vec::new();
        let mut flagged = [0; 3];
        let base = encode_with(code, origin, is64, &mut |r, at, v| {
            let i = flags_of(r) - CALLF;
            // (of the fields within the section: the encoder reads its last bytes from a copy
            // padded with zeros, which the decoder cuts off)
            let within = at.wrapping_sub(origin).checked_add(4).is_some_and(|e| e <= code.len() as u64);
            let r = remap.get(v & width(is64)).filter(|_| within);
            flags[i].push(r.is_some());
            let Some(r) = r else { return v };
            flagged[i] += 1;
            pairs.push((r, v & width(is64)));
            r
        })?;
        pairs.sort_unstable();
        pairs.dedup();
        debug_assert!(pairs.windows(2).all(|w| w[0].0 != w[1].0), "a value mapped twice");
        let (mut map, mut last) = (Vec::new(), 0u64);
        for &(v, t) in &pairs {
            let s = t.wrapping_sub(v) & width(is64);
            let d = s.wrapping_sub(last) & width(is64);
            last = s;
            // the difference sign-extended from the width, zigzag coded, as a LEB128
            let d = if is64 { d as i64 } else { d as u32 as i32 as i64 };
            let mut z = ((d << 1) ^ (d >> 63)) as u64;
            while z >= 0x80 {
                map.push(z as u8 | 0x80);
                z >>= 7;
            }
            map.push(z as u8);
        }
        let n = [0, 1, 2].map(|i| flags[i].n);
        let [c, j, a] = flags.map(|b| b.out);
        Some(EncodedStreams { base, streams: [c, j, a, map], n, flagged })
    }
}

#[cfg(feature = "encode")]
pub use encode::*;

#[cfg(feature = "decode")]
mod decode {
    use super::super::{decode_hooked, DecodedStreams, Opcodes};
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

    /// The shifts of the values `vals` (sorted, distinct) from map `s`, of x86-64 or x86-32.
    fn shifts(vals: &[u64], mut s: &[u8], is64: bool) -> Option<Vec<u64>> {
        let w = width(is64);
        let (mut out, mut last) = (Vec::with_capacity(vals.len()), 0u64);
        for _ in vals {
            let mut z = 0u64;
            for sh in (0..70).step_by(7) {
                let (&x, rest) = s.split_first()?;
                s = rest;
                if sh == 63 && x > 1 {
                    return None;
                }
                z |= ((x & 0x7f) as u64) << sh;
                if x < 0x80 {
                    break;
                }
            }
            let d = (z >> 1) ^ (z & 1).wrapping_neg();
            if !is64 && d as i64 != d as i32 as i64 {
                return None;
            }
            last = last.wrapping_add(d) & w;
            out.push(last);
        }
        s.is_empty().then_some(out)
    }

    /// The section from its `streams` (as [`super::super::decode_with`] takes them, then those
    /// from [`CALLF`] on), at most `capacity` bytes reserved; `None` where they do not make one.
    pub fn decode(
        origin: u64,
        sentinel_size: u8,
        streams: [&[u8]; NUM_STREAMS],
        is64: bool,
        opcodes: &Opcodes,
        capacity: usize,
    ) -> Option<Vec<u8>> {
        let w = width(is64);
        let base = DecodedStreams::new(origin, sentinel_size, core::array::from_fn(|i| streams[i]));
        let mut flags = [CALLF, JUMPF, ADDRF].map(|i| Flags { s: streams[i], n: 0 });
        // of each reference flagged, where its field is and its value (as many as the flags set,
        // at most: no more memory than that)
        let ones: usize = flags.iter().flat_map(|f| f.s).map(|b| b.count_ones() as usize).sum();
        let mut fixups: Vec<(u64, usize)> = Vec::with_capacity(ones);
        let mut out = decode_hooked(&base, is64, opcodes, capacity, |r, v, at| {
            if flags[flags_of(r) - CALLF].next()? {
                fixups.push((v & w, at));
            }
            Some(())
        })?;
        if !flags.iter().all(Flags::done) {
            return None;
        }
        fixups.sort_unstable();
        let mut vals: Vec<u64> = fixups.iter().map(|f| f.0).collect();
        vals.dedup();
        let shifts = shifts(&vals, streams[MAP], is64)?;
        let mut j = 0;
        for &(v, at) in &fixups {
            while vals[j] != v {
                j += 1;
            }
            let f = out.get_mut(at..at.checked_add(4)?)?;
            let x = u32::from_le_bytes(f.try_into().unwrap()).wrapping_add(shifts[j] as u32);
            f.copy_from_slice(&x.to_le_bytes());
        }
        Some(out)
    }
}

#[cfg(feature = "decode")]
pub use decode::decode;

#[cfg(all(test, feature = "encode", feature = "decode", not(target_family = "wasm")))]
mod tests {
    use super::super::Opcodes;
    use super::*;
    use alloc::vec;

    fn streams(e: &EncodedStreams) -> [&[u8]; NUM_STREAMS] {
        core::array::from_fn(|i| if i < NUM_BASE { &e.base.streams()[i][..] } else { &e.extra()[i - NUM_BASE][..] })
    }

    fn roundtrip(code: &[u8], origin: u64, is64: bool, remap: &Remap) -> EncodedStreams {
        let e = encode(code, origin, is64, remap).unwrap();
        let out = decode(e.base.origin(), e.base.sentinel_size(), streams(&e), is64, &Opcodes::BUILTIN, 0);
        assert_eq!(out.as_deref(), Some(code));
        e
    }

    /// Functions of distinct bodies, each calling another, taking an address of its own and
    /// jumping to the next, padded to 16 bytes, after `gap` bytes of padding; at `origin`.
    fn image(n: usize, order: &[usize], gap: usize, origin: u64) -> Vec<u8> {
        let len = 32;
        let mut at = vec![0; n];
        for (k, &f) in order.iter().enumerate() {
            at[f] = gap + k * len;
        }
        let mut c = vec![0xcc; gap];
        for &f in order {
            let start = c.len();
            let rel = |c: &Vec<u8>, to: usize| (to as i64 - (c.len() + 4) as i64) as i32;
            c.extend_from_slice(&[0x55, 0x48, 0x89, 0xe5, 0xe8]); // push rbp; mov rbp, rsp; call
            let r = rel(&c, at[(f * 7 + 3) % n]);
            c.extend_from_slice(&r.to_le_bytes());
            c.extend_from_slice(&[0x48, 0x8d, 0x05]); // lea rax, [rip + data f]
            let data = 0x10_0000 + 64 * f as u64;
            let r = data as i64 - (origin as i64 + c.len() as i64 + 4);
            c.extend_from_slice(&(r as i32).to_le_bytes());
            c.push(0xb9); // mov ecx, f
            c.extend_from_slice(&(f as u32 * 0x01010101).to_le_bytes());
            c.push(0xe9); // jmp the next one
            let r = rel(&c, at[(f + 1) % n]);
            c.extend_from_slice(&r.to_le_bytes());
            c.extend_from_slice(&[0x5d, 0xc3]); // pop rbp; ret
            c.resize(start + len, 0xcc);
        }
        c
    }

    #[test]
    fn moved_functions() {
        let order: Vec<usize> = (0..60).collect();
        let a = image(60, &order, 0, 0x1000);
        let mut ob = order.clone();
        ob.swap(7, 40);
        let b = image(60, &ob, 48, 0x1000);
        let r = Remap::new(&[(0x1000, &a)], &[(0x1000, &b)], true);
        assert!(r.stats.matched > 50, "{:?}", r.stats);
        let e = roundtrip(&b, 0x1000, true, &r);
        let [c, j, d] = e.flagged();
        // (the addresses of data that did not move too, to themselves)
        assert!([c, j, d].iter().all(|f| f.1 * 10 > f.0 * 8), "{c:?} {j:?} {d:?}");
        // B's calls and jumps are A's
        let ea = super::super::encode(&a, 0x1000, true).unwrap();
        for st in [super::super::ST_CALL, super::super::ST_JUMP] {
            let (sa, sb) = (&ea.streams()[st], &e.base.streams()[st]);
            let mut wa: Vec<&[u8]> = sa.chunks(8).collect();
            wa.sort_unstable();
            let found = sb.chunks(8).filter(|w| wa.binary_search(w).is_ok()).count();
            assert!(found * 10 >= sb.len() / 8 * 9, "{found} of {}", sb.len() / 8);
        }
    }

    /// Maps built by hand, of some of the values the code holds; any references round-trip.
    #[test]
    fn any_maps() {
        let mut x = 0x1234_5678_9abc_def0u64;
        let code: Vec<u8> = (0..60_003)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                x as u8
            })
            .collect();
        for (is64, origin) in [(true, 0x1_0000_1000), (false, 0x40_1000), (true, 0xffff_ffff_ffff_f000), (false, 0)] {
            let mut vals = Vec::new();
            super::super::encode_with(&code, origin, is64, &mut |_, _, v| {
                vals.push(v & width(is64));
                v
            })
            .unwrap();
            vals.sort_unstable();
            vals.dedup();
            let map: Vec<(u64, u64)> = vals
                .iter()
                .filter(|&&v| v % 3 != 0)
                .map(|&v| (v, v.wrapping_mul(0x9e37_79b9_7f4a_7c15) & width(is64)))
                .collect();
            let e = roundtrip(&code, origin, is64, &Remap { map, stats: Stats::default() });
            assert!(e.flagged().iter().map(|f| f.1).sum::<usize>() > 100, "{:?}", e.flagged());
        }
    }

    #[test]
    fn corrupt_streams_fail() {
        let order: Vec<usize> = (0..20).collect();
        let a = image(20, &order, 0, 0x1000);
        let b = image(20, &order, 16, 0x1000);
        let r = Remap::new(&[(0x1000, &a)], &[(0x1000, &b)], true);
        let e = roundtrip(&b, 0x1000, true, &r);
        let s = streams(&e);
        let run =
            |s: [&[u8]; NUM_STREAMS]| decode(e.base.origin(), e.base.sentinel_size(), s, true, &Opcodes::BUILTIN, 0);
        // a flag too many, a flag too few
        let (more, fewer) = ([s[CALLF], &[0x80]].concat(), &s[CALLF][..s[CALLF].len() - 1]);
        for f in [&more[..], fewer] {
            let mut t = s;
            t[CALLF] = f;
            assert_eq!(run(t), None);
        }
        // a map short of a value, or with one too many
        let (short, long) = (&s[MAP][..s[MAP].len() - 1], [s[MAP], &[0]].concat());
        for m in [short, &long[..]] {
            let mut t = s;
            t[MAP] = m;
            assert_eq!(run(t), None);
        }
    }
}
