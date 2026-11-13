//! ETC1's, ETC2's and EAC's blocks (FORMAT.md, "`etc-recons`"): each a 64-bit big-endian word.
//! An RGB block of ETC1 or ETC2 is of a *mode* (individual, differential, and ETC2's T, H and
//! planar, by the overflows of its differential fields), each of its fields ([`FIELDS`]); the
//! bits of no field of the mode (those that only make the overflows) are kept as they are, so
//! that any word is the fields of its mode and those bits. An EAC block is a base, a multiplier,
//! a table and 3-bit indices. What a block decodes to predicts only, as the planes hold it.

use crate::layout::Format;

/// The modes of an RGB block.
pub const INDIVIDUAL: u8 = 0;
pub const DIFFERENTIAL: u8 = 1;
pub const T: u8 = 2;
pub const H: u8 = 3;
pub const PLANAR: u8 = 4;

/// The fields of each mode, each its segments of the word (the lowest bit, the bits), from the
/// most significant: individual R1 R2 G1 G2 B1 B2 cw1 cw2 flip; differential R dR G dG B dB cw1
/// cw2 flip; T R1 G1 B1 R2 G2 B2 dist; H R1 G1 B1 R2 G2 B2 da db; planar RO GO BO RH GH BH RV GV
/// BV. Bit 33 (ETC1's and ETC2's mode, punch-through's opacity) and the indices (bits 0 to 31,
/// but of planar blocks) are apart.
pub type Field = &'static [(u8, u8)];

pub const FIELDS: [&[Field]; 5] = [
    &[&[(60, 4)], &[(56, 4)], &[(52, 4)], &[(48, 4)], &[(44, 4)], &[(40, 4)], &[(37, 3)], &[(34, 3)], &[(32, 1)]],
    &[&[(59, 5)], &[(56, 3)], &[(51, 5)], &[(48, 3)], &[(43, 5)], &[(40, 3)], &[(37, 3)], &[(34, 3)], &[(32, 1)]],
    &[&[(59, 2), (56, 2)], &[(52, 4)], &[(48, 4)], &[(44, 4)], &[(40, 4)], &[(36, 4)], &[(34, 2), (32, 1)]],
    &[&[(59, 4)], &[(56, 3), (52, 1)], &[(51, 1), (47, 3)], &[(43, 4)], &[(39, 4)], &[(35, 4)], &[(34, 1)], &[(32, 1)]],
    &[
        &[(57, 6)],
        &[(56, 1), (49, 6)],
        &[(48, 1), (43, 2), (39, 3)],
        &[(34, 5), (32, 1)],
        &[(25, 7)],
        &[(19, 6)],
        &[(13, 6)],
        &[(6, 7)],
        &[(0, 6)],
    ],
];

/// The bits of each mode's fields, bit 33 and its indices: what is left of the word are the
/// mode's bits *left over*.
pub const fn used(mode: u8) -> u64 {
    let fields = FIELDS[mode as usize];
    let mut m: u64 = 1 << 33;
    if mode != PLANAR {
        m |= 0xffff_ffff;
    }
    let mut i = 0;
    while i < fields.len() {
        let mut j = 0;
        while j < fields[i].len() {
            let (lo, n) = fields[i][j];
            m |= ((1u64 << n) - 1) << lo;
            j += 1;
        }
        i += 1;
    }
    m
}

/// Field `f`'s value in word `w` (its segments from the most significant).
#[inline]
pub fn get(w: u64, f: Field) -> u32 {
    f.iter().fold(0, |v, &(lo, n)| v << n | (w >> lo) as u32 & ((1 << n) - 1))
}

/// Word `w` with field `f` set to `v`.
#[inline]
pub fn set(w: u64, f: Field, v: u32) -> u64 {
    let mut shift: u32 = f.iter().map(|s| s.1 as u32).sum();
    let mut w = w;
    for &(lo, n) in f {
        shift -= n as u32;
        let mask = ((1u64 << n) - 1) << lo;
        w = w & !mask | ((v >> shift) as u64) << lo & mask;
    }
    w
}

/// The bits of field `f`.
pub fn bits(f: Field) -> u32 {
    f.iter().map(|s| s.1 as u32).sum()
}

/// Whether the format's bit 33 is the opacity of punch-through alpha (ETC2's RGB A1), not the
/// mode.
pub fn punch(f: Format) -> bool {
    f == Format::Etc2P
}

/// The mode of RGB word `w` of format `f` (ETC1: individual or differential only).
pub fn mode_of(w: u64, f: Format) -> u8 {
    if !punch(f) && w >> 33 & 1 == 0 {
        return INDIVIDUAL;
    }
    if f == Format::Etc1 {
        return DIFFERENTIAL;
    }
    // (a channel of 5 bits above lo + 3, its difference of 3 bits above lo, out of 0 to 31)
    let over = |lo: u32| {
        let (b, d) = ((w >> (lo + 3)) as i32 & 31, ((w >> lo) as i32 & 7) << 29 >> 29);
        !(0..32).contains(&(b + d))
    };
    if over(56) {
        T
    } else if over(48) {
        H
    } else if over(40) {
        PLANAR
    } else {
        DIFFERENTIAL
    }
}

/// The modifiers of ETC1's tables (and ETC2's individual and differential modes): a and b of each
/// codeword, index 0 giving +a, 1 +b, 2 −a, 3 −b.
pub const MODIFIERS: [[i32; 2]; 8] = [[2, 8], [5, 17], [9, 29], [13, 42], [18, 60], [24, 80], [33, 106], [47, 183]];

/// The distances of the T and H modes.
pub const DISTANCES: [i32; 8] = [3, 6, 11, 16, 23, 32, 41, 64];

/// EAC's modifiers of each table, by index.
pub const EAC: [[i32; 8]; 16] = [
    [-3, -6, -9, -15, 2, 5, 8, 14],
    [-3, -7, -10, -13, 2, 6, 9, 12],
    [-2, -5, -8, -13, 1, 4, 7, 12],
    [-2, -4, -6, -13, 1, 3, 5, 12],
    [-3, -6, -8, -12, 2, 5, 7, 11],
    [-3, -7, -9, -11, 2, 6, 8, 10],
    [-4, -7, -8, -11, 3, 6, 7, 10],
    [-3, -5, -8, -11, 2, 4, 7, 10],
    [-2, -6, -8, -10, 1, 5, 7, 9],
    [-2, -5, -8, -10, 1, 4, 7, 9],
    [-2, -4, -8, -10, 1, 3, 7, 9],
    [-2, -5, -7, -10, 1, 4, 6, 9],
    [-3, -4, -7, -10, 2, 3, 6, 9],
    [-1, -2, -3, -10, 0, 1, 2, 9],
    [-4, -6, -8, -9, 3, 5, 7, 8],
    [-3, -5, -7, -9, 2, 4, 6, 8],
];

/// The bit of pixel p (= 4y + x, row by row) in the indices of a block, which go column by column.
#[inline(always)]
pub fn column(p: usize) -> usize {
    (p % 4) * 4 + p / 4
}

/// A 4-bit channel expanded to 8 bits; a 5-bit one; 6 and 7.
#[inline(always)]
pub fn ext(v: i32, bits: u32) -> i32 {
    match bits {
        4 => v * 17,
        5 => v << 3 | v >> 2,
        6 => v << 2 | v >> 4,
        _ => v << 1 | v >> 6,
    }
}

/// The colours of an RGB block of the individual or differential mode: its two base colours (8
/// bits), its two codewords and its flip bit.
pub fn bases(w: u64, mode: u8) -> ([[i32; 3]; 2], [usize; 2], bool) {
    let f = FIELDS[mode as usize];
    let g = |i: usize| get(w, f[i]) as i32;
    let b = match mode {
        INDIVIDUAL => [[g(0), g(2), g(4)].map(|v| ext(v, 4)), [g(1), g(3), g(5)].map(|v| ext(v, 4))],
        _ => {
            let base = [g(0), g(2), g(4)];
            let delta = [g(1), g(3), g(5)].map(|d| d << 29 >> 29);
            let second: [i32; 3] = std::array::from_fn(|c| (base[c] + delta[c]) & 31);
            [base.map(|v| ext(v, 5)), second.map(|v| ext(v, 5))]
        }
    };
    (b, [g(6) as usize, g(7) as usize], g(8) != 0)
}

/// The subblock of pixel p (0 or 1) of a block of `flip`.
#[inline(always)]
pub fn subblock(p: usize, flip: bool) -> usize {
    if flip { (p / 4 >= 2) as usize } else { (p % 4 >= 2) as usize }
}

/// The paint colours of a T or H block.
pub fn paints(w: u64, mode: u8) -> [[i32; 3]; 4] {
    let f = FIELDS[mode as usize];
    let g = |i: usize| get(w, f[i]) as i32;
    let (c1, c2) = ([g(0), g(1), g(2)], [g(3), g(4), g(5)]);
    let (e1, e2) = (c1.map(|v| ext(v, 4)), c2.map(|v| ext(v, 4)));
    let clamp = |c: [i32; 3], d: i32| c.map(|v| (v + d).clamp(0, 255));
    if mode == T {
        let d = DISTANCES[g(6) as usize];
        [e1, clamp(e2, d), e2, clamp(e2, -d)]
    } else {
        let order = (c1[0] << 8 | c1[1] << 4 | c1[2]) >= (c2[0] << 8 | c2[1] << 4 | c2[2]);
        let d = DISTANCES[(g(6) << 2 | g(7) << 1 | order as i32) as usize];
        [clamp(e1, d), clamp(e1, -d), clamp(e2, d), clamp(e2, -d)]
    }
}

/// The colours O, H and V (8 bits) of a planar block.
pub fn planes(w: u64) -> [[i32; 3]; 3] {
    let f = FIELDS[PLANAR as usize];
    let g = |i: usize| get(w, f[i]) as i32;
    std::array::from_fn(|k| [ext(g(3 * k), 6), ext(g(3 * k + 1), 7), ext(g(3 * k + 2), 6)])
}

/// Pixel (x, y) of a planar block of colours `o`.
#[inline(always)]
pub fn planar(o: &[[i32; 3]; 3], x: i32, y: i32) -> [i32; 3] {
    std::array::from_fn(|c| ((x * (o[1][c] - o[0][c]) + y * (o[2][c] - o[0][c]) + 4 * o[0][c] + 2) >> 2).clamp(0, 255))
}

/// The index of pixel p (row by row) of word `w`.
#[inline(always)]
pub fn index(w: u64, p: usize) -> u32 {
    let k = column(p);
    ((w >> (16 + k)) as u32 & 1) << 1 | (w >> k) as u32 & 1
}

/// The colour of index `i` of a block of the individual or differential mode, of base `base` and
/// codeword `cw` (punch-through of no opacity: index 0 the base, 2 transparent), and its alpha.
#[inline(always)]
pub fn modified(base: [i32; 3], cw: usize, i: u32, opaque: bool) -> [u8; 4] {
    let [a, b] = MODIFIERS[cw];
    let m = match (i, opaque) {
        (0, true) => a,
        (0, false) => 0,
        (1, _) => b,
        (2, true) => -a,
        (2, false) => return [0; 4],
        _ => -b,
    };
    let c = base.map(|v| (v + m).clamp(0, 255) as u8);
    [c[0], c[1], c[2], 255]
}

/// The pixels of RGB word `w` of format `f`: RGBA, alpha 255 but of punch-through's transparent
/// pixels (all 0).
pub fn pixels(w: u64, f: Format) -> [[u8; 4]; 16] {
    let mode = mode_of(w, f);
    let opaque = !punch(f) || w >> 33 & 1 != 0;
    let trans = |i: u32| !opaque && i == 2;
    match mode {
        INDIVIDUAL | DIFFERENTIAL => {
            let (b, cw, flip) = bases(w, mode);
            std::array::from_fn(|p| {
                let s = subblock(p, flip);
                modified(b[s], cw[s], index(w, p), opaque)
            })
        }
        T | H => {
            let pc = paints(w, mode);
            std::array::from_fn(|p| {
                let i = index(w, p);
                if trans(i) {
                    return [0; 4];
                }
                let c = pc[i as usize];
                [c[0] as u8, c[1] as u8, c[2] as u8, 255]
            })
        }
        _ => {
            let o = planes(w);
            std::array::from_fn(|p| {
                let c = planar(&o, (p % 4) as i32, (p / 4) as i32);
                [c[0] as u8, c[1] as u8, c[2] as u8, 255]
            })
        }
    }
}

/// What an EAC block's values are: ETC2's alpha (8 bits), or R11 (11 bits) unsigned or signed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Eac {
    Alpha,
    R11,
    R11Signed,
}

impl Eac {
    /// The value of index `i` of an EAC block of base `b`, multiplier `m` and table `t`: of R11,
    /// its 11 bits (signed: plus 1024).
    #[inline(always)]
    pub fn value(self, b: u32, m: u32, t: u32, i: u32) -> i32 {
        let md = EAC[t as usize][i as usize];
        let m = m as i32;
        match self {
            Eac::Alpha => (b as i32 + md * m).clamp(0, 255),
            Eac::R11 => {
                let v = b as i32 * 8 + 4 + if m == 0 { md } else { md * m * 8 };
                v.clamp(0, 2047)
            }
            Eac::R11Signed => {
                let b = (b as u8 as i8 as i32).max(-127);
                let v = b * 8 + if m == 0 { md } else { md * m * 8 };
                v.clamp(-1023, 1023) + 1024
            }
        }
    }

    /// The values of the indices of EAC word `w`.
    pub fn values(self, w: u64) -> [i32; 8] {
        let (b, m, t) = ((w >> 56) as u32, (w >> 52) as u32 & 15, (w >> 48) as u32 & 15);
        std::array::from_fn(|i| self.value(b, m, t, i as u32))
    }

    /// The pixels of EAC word `w`, row by row.
    pub fn pixels(self, w: u64) -> [u16; 16] {
        let v = self.values(w);
        std::array::from_fn(|p| v[(w >> (45 - 3 * column(p))) as usize & 7] as u16)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fields() {
        // each mode's fields and its bits left over are its word, of every format
        let mut x = 0x0bad_cafe_dead_beef_u64;
        for f in [Format::Etc1, Format::Etc2, Format::Etc2P] {
            let mut seen = [0; 5];
            for _ in 0..50000 {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                let mode = mode_of(x, f);
                seen[mode as usize] += 1;
                let mut w = x & !used(mode) | x & 1 << 33;
                if mode != PLANAR {
                    w |= x & 0xffff_ffff;
                }
                for &fl in FIELDS[mode as usize] {
                    w = set(w, fl, get(x, fl));
                }
                assert_eq!(w, x, "{f:?} {mode}");
            }
            match f {
                Format::Etc1 => assert!(seen[0] > 0 && seen[1] > 0 && seen[2..].iter().all(|&n| n == 0)),
                Format::Etc2 => assert!(seen.iter().all(|&n| n > 0)),
                _ => assert!(seen[0] == 0 && seen[1..].iter().all(|&n| n > 0)),
            }
        }
        // the fields of a mode are apart
        for mode in 0..5 {
            let mut m = 0u64;
            for &f in FIELDS[mode as usize] {
                for &(lo, n) in f {
                    let s = ((1u64 << n) - 1) << lo;
                    assert_eq!(m & s, 0, "{mode}");
                    m |= s;
                }
            }
        }
    }
}
