//! ARM64 (A64) code: the instructions stay in one stream, as big-endian words (their opcode
//! bits first), with the targets of BL and ADRP made absolute, so that calls to one function
//! and references to one page look alike. The offsets of local branches (B, B.cond, CBZ,
//! CBNZ, TBZ, TBNZ) are taken out of the words, which then repeat, and go zigzag coded to
//! streams of their own, a byte of each in each. The section is rebuilt bit for bit.
//!
//! Splitting every instruction into its fields by encoding class, as the x86 filter does,
//! was tried and archives larger under zstd: the words repeat as a whole (prologues, calls,
//! loads off one base) and lose their matches once their fields are apart.

use alloc::vec::Vec;

macro_rules! define_streams {
    ($($i:ident),* $(,)?) => (
        define_streams! { @0 $($i)* }
        /// Names of the streams, in order.
        pub const STREAM_NAMES: [&str; NUM_STREAMS] = [$(stringify!($i)),*];
    );

    (@$c:tt $i:ident $($t:tt)*) => (
        #[allow(dead_code)] const $i: usize = $c;
        define_streams! { @(1 + $c) $($t)* }
    );

    (@$c:tt) => (
        /// Number of the streams; all of them are always present, possibly empty.
        pub const NUM_STREAMS: usize = $c;
    );
}

define_streams! {
    CODE,
    B3, B2, B1, B0,
    BCOND2, BCOND1, BCOND0,
    TBZ1, TBZ0,
    TAIL,
}

/// What an instruction's word is made of in [`CODE`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// As it is.
    Other,
    /// BL: `imm26` (bits 25..0) made absolute, in words, modulo 2^26.
    Bl,
    /// ADRP: `immhi:immlo` (bits 23..5, 30..29) made absolute, in pages, modulo 2^21.
    Adrp,
    /// A local branch: its offset field, of `bits` bits from bit `lo`, cleared, and the
    /// offset, zigzag coded, to the streams from `first` on, a byte in each from the most
    /// significant.
    Local { lo: u32, bits: u32, first: usize },
}

fn kind(w: u32) -> Kind {
    if w & 0xfc00_0000 == 0x9400_0000 {
        Kind::Bl
    } else if w & 0x9f00_0000 == 0x9000_0000 {
        Kind::Adrp
    } else if w & 0xfc00_0000 == 0x1400_0000 {
        // B
        Kind::Local { lo: 0, bits: 26, first: B3 }
    } else if w & 0xff00_0000 == 0x5400_0000 || w & 0x7e00_0000 == 0x3400_0000 {
        // B.cond and BC.cond; CBZ and CBNZ
        Kind::Local { lo: 5, bits: 19, first: BCOND2 }
    } else if w & 0x7e00_0000 == 0x3600_0000 {
        // TBZ and TBNZ
        Kind::Local { lo: 5, bits: 14, first: TBZ1 }
    } else {
        Kind::Other
    }
}

const fn low(bits: u32) -> u32 {
    u32::MAX >> (32 - bits)
}

/// The ADRP immediate of `w`, `immhi:immlo`.
fn adrp_imm(w: u32) -> u32 {
    (w >> 3 & 0x001f_fffc) | (w >> 29 & 3)
}

fn with_adrp_imm(w: u32, imm: u32) -> u32 {
    (w & 0x9f00_001f) | (imm & 3) << 29 | (imm >> 2 & 0x7_ffff) << 5
}

/// `w` with its target made absolute (`pc` the address of the instruction), or back
/// relative (`sign` -1).
fn retarget(w: u32, k: Kind, pc: u64, sign: u32) -> u32 {
    match k {
        Kind::Bl => (w & !low(26)) | (w.wrapping_add(sign.wrapping_mul((pc >> 2) as u32)) & low(26)),
        Kind::Adrp => with_adrp_imm(w, adrp_imm(w).wrapping_add(sign.wrapping_mul((pc >> 12) as u32))),
        _ => w,
    }
}

/// Bytes of a zigzag coded offset of `bits` bits.
const fn offset_bytes(bits: u32) -> usize {
    bits.div_ceil(8) as usize
}

#[cfg(feature = "encode")]
mod encode {
    use super::*;
    use core::array;

    /// The streams of a section, as [`encode`] gives them.
    #[derive(Debug)]
    pub struct EncodedStreams {
        streams: [Vec<u8>; NUM_STREAMS],
    }

    impl EncodedStreams {
        pub fn streams(&self) -> &[Vec<u8>; NUM_STREAMS] {
            &self.streams
        }
    }

    /// The streams of the code section `code` whose first byte is at address `origin`.
    pub fn encode(code: &[u8], origin: u64) -> EncodedStreams {
        let mut st: [Vec<u8>; NUM_STREAMS] = array::from_fn(|_| Vec::new());
        st[CODE].reserve(code.len());
        let words = code.chunks_exact(4);
        st[TAIL].extend_from_slice(words.remainder());
        for (k, w) in words.enumerate() {
            let mut w = u32::from_le_bytes(w.try_into().unwrap());
            let pc = origin.wrapping_add(4 * k as u64);
            match kind(w) {
                Kind::Other => {}
                Kind::Local { lo, bits, first } => {
                    let v = w >> lo & low(bits);
                    w &= !(low(bits) << lo);
                    let s = ((v << (32 - bits)) as i32) >> (32 - bits);
                    let z = ((s << 1) ^ (s >> 31)) as u32;
                    let n = offset_bytes(bits);
                    for (b, s) in st[first..first + n].iter_mut().enumerate() {
                        s.push((z >> (8 * (n - 1 - b))) as u8);
                    }
                }
                k => w = retarget(w, k, pc, 1),
            }
            st[CODE].extend_from_slice(&w.to_be_bytes());
        }
        EncodedStreams { streams: st }
    }
}

#[cfg(feature = "encode")]
pub use encode::*;

#[cfg(feature = "decode")]
mod decode {
    use super::*;

    fn take<'a>(s: &mut &'a [u8], n: usize) -> Option<&'a [u8]> {
        let (a, b) = (s.get(..n)?, s.get(n..)?);
        *s = b;
        Some(a)
    }

    /// The section of `len` bytes at address `origin` from its `streams`; `None` where they
    /// do not make one, or are not all read.
    pub fn decode(streams: [&[u8]; NUM_STREAMS], origin: u64, len: usize) -> Option<Vec<u8>> {
        let mut s = streams;
        let mut out = Vec::with_capacity(len);
        for k in 0..len / 4 {
            let mut w = u32::from_be_bytes(take(&mut s[CODE], 4)?.try_into().unwrap());
            let pc = origin.wrapping_add(4 * k as u64);
            match kind(w) {
                Kind::Other => {}
                Kind::Local { lo, bits, first } => {
                    if w >> lo & low(bits) != 0 {
                        return None;
                    }
                    let mut z = 0u32;
                    for st in &mut s[first..first + offset_bytes(bits)] {
                        z = z << 8 | take(st, 1)?[0] as u32;
                    }
                    if z >> bits != 0 {
                        return None;
                    }
                    let v = (z >> 1) ^ (z & 1).wrapping_neg();
                    w |= (v & low(bits)) << lo;
                }
                k => w = retarget(w, k, pc, u32::MAX),
            }
            out.extend_from_slice(&w.to_le_bytes());
        }
        out.extend_from_slice(take(&mut s[TAIL], len % 4)?);
        s.iter().all(|s| s.is_empty()).then_some(out)
    }
}

#[cfg(feature = "decode")]
pub use decode::decode;

#[cfg(test)]
mod tests {
    use super::*;
    use core::array;

    fn roundtrip(code: &[u8], origin: u64) -> EncodedStreams {
        let e = encode(code, origin);
        let s: [&[u8]; NUM_STREAMS] = array::from_fn(|i| &e.streams()[i][..]);
        assert_eq!(decode(s, origin, code.len()).as_deref(), Some(code));
        e
    }

    fn code(words: &[u32]) -> Vec<u8> {
        words.iter().flat_map(|w| w.to_le_bytes()).collect()
    }

    #[test]
    fn known_instructions() {
        let origin = 0x1_0000_4000;
        let words = [
            0xa9bf7bfd, // stp x29, x30, [sp, #-16]!
            0x94000010, // bl +0x40
            0x90000008, // adrp x8, +0 pages
            0xb4000048, // cbz x8, +8
            0x54ffffe1, // b.ne -4
            0x17fffffb, // b -20
            0x37480060, // tbnz w0, #9, +12
            0xd65f03c0, // ret
        ];
        let e = roundtrip(&code(&words), origin);
        let cw: Vec<u32> = e.streams()[CODE].chunks(4).map(|c| u32::from_be_bytes(c.try_into().unwrap())).collect();
        assert_eq!(cw[0], words[0]);
        // bl from origin + 4: the absolute target in words
        assert_eq!(cw[1], 0x9400_0000 | (((origin + 4 + 0x40) >> 2) as u32 & low(26)));
        // adrp at origin + 8: the absolute page
        assert_eq!(adrp_imm(cw[2]), ((origin >> 12) as u32) & low(21));
        // the local branches without their offsets, which are zigzag coded apart
        assert_eq!(cw[3..7], [0xb4000008, 0x54000001, 0x14000000, 0x37480000]);
        assert_eq!(e.streams()[BCOND0], [4, 1]); // +2 and -1 words
        assert_eq!(e.streams()[BCOND2], [0, 0]);
        assert_eq!(e.streams()[B0], [9]); // -5 words
        assert_eq!(e.streams()[TBZ0], [6]); // +3 words
        assert_eq!(cw[7], words[7]);
    }

    /// Every offset, positive and negative, at both ends of its field.
    #[test]
    fn offsets_at_extremes() {
        let mut words = Vec::new();
        for (base, bits, lo) in
            [(0x1400_0000u32, 26, 0), (0x5400_0000, 19, 5), (0xb500_0000, 19, 5), (0x3600_0000, 14, 5)]
        {
            for v in [0, 1, low(bits), low(bits) >> 1, (low(bits) >> 1) + 1] {
                words.push(base | v << lo);
            }
        }
        for v in [0, 1, low(26)] {
            words.push(0x9400_0000 | v);
        }
        for imm in [0, 1, low(21), 0x10_0000] {
            words.push(with_adrp_imm(0x9000_0003, imm));
        }
        for origin in [0, 0xffff_ffff_ffff_f000, 0x1234_5678] {
            roundtrip(&code(&words), origin);
        }
    }

    #[test]
    fn any_words() {
        let mut x = 0x1234_5678_9abc_def0u64;
        let data: Vec<u8> = (0..40_003)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                x as u8
            })
            .collect();
        for origin in [0, 2, 0xffff_ffff_ffff_fff0] {
            roundtrip(&data, origin);
            roundtrip(&data[..7], origin);
        }
        roundtrip(&[], 0);
    }

    #[test]
    fn corrupt_streams_fail() {
        let c = code(&[0x94000010, 0x54000041]);
        let e = encode(&c, 0);
        let s: [&[u8]; NUM_STREAMS] = array::from_fn(|i| &e.streams()[i][..]);
        // a byte short of the tail, streams left over
        assert_eq!(decode(s, 0, c.len() - 1), None);
        assert_eq!(decode(s, 0, c.len() - 4), None);
        // an offset left in a local branch's word
        let mut bad = e.streams()[CODE].clone();
        bad[7] |= 0x20;
        let mut t = s;
        t[CODE] = &bad;
        assert_eq!(decode(t, 0, c.len()), None);
        // a zigzag value past its field
        let big = [0x08];
        let mut t = s;
        t[BCOND2] = &big;
        assert_eq!(decode(t, 0, c.len()), None);
    }
}
