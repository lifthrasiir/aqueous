//! Numbers printed as ASCII text, as integers plus how they were printed.
//!
//! A number token is a descriptor (two bytes: mode and flags) and a decimal `D`, given as an
//! integer `v` at a scale `s` (`D` = `v` × 10^-`s`, `s` in `MIN_SCALE..=MAX_SCALE`). The
//! token is a function of the descriptor and `D` alone, the same on every platform: what
//! the encoder chose (`style`) is never needed to decode. `README.md` specifies it.
//!
//! - [`fixed`]: `%.Nf`-like tokens (the scale is in the flags), scales.
//! - [`general`]: C's `%.Pg` of decimals.
//! - [`dec`]: decimals and binary floats; the renders (`%g`, `%e`, Python `repr`,
//!   JavaScript `toString`, positional), and the coding of a token in a given mode.
//! - [`style`]: the encoder's choice of mode for a group of numbers, and its fallbacks.
//!
//! ```
//! use aqr_num::style::{Style, choose};
//! let toks: Vec<(usize, &[u8])> = [&b"0.10000000000000001"[..], b"12.759518999999999", b"-2.470218"]
//!     .into_iter()
//!     .enumerate()
//!     .collect();
//! let st: Style = choose(&toks);
//! let (v, s, mode, f) = st.encode(b"0.10000000000000001", 0, &mut vec![]).unwrap();
//! assert_eq!((v, s), (1, 1)); // 0.1, printed with %.17g from its double
//! let mut out = vec![];
//! aqr_num::put(&mut out, v, s, mode, f);
//! assert_eq!(out, b"0.10000000000000001");
//! ```

pub mod dec;
pub mod fixed;
pub mod general;
pub mod style;

pub use dec::{encode, mode, put, render_of, src_of, valid};
pub use fixed::{MAX_SCALE, MIN_SCALE};

/// Fixed (`%.Nf`-like) numbers: their scale is in the flags.
pub const M_FIX: u8 = dec::mode(dec::R_FIX, dec::SRC_DEC);
/// C's `%.Pg` of decimals.
pub const M_GEN: u8 = dec::mode(dec::R_GEN, dec::SRC_DEC);

/// A readable form of a descriptor (one `valid` accepts), for analysis: fixed numbers by
/// their digits after the point ("i" for none, "#" without an integer part, "-" for a
/// negative zero), others as `[f32:|f64:]render[P][E][-]` ("E" for three-digit exponents).
pub fn describe(mode: u8, f: u8) -> String {
    if mode == M_FIX {
        let mut s = String::new();
        if f & fixed::F_NEGZ != 0 {
            s += "-";
        }
        if f & fixed::F_NOINT != 0 {
            s += "#";
        }
        return s + &if f & fixed::F_POINT != 0 { format!("{}", f & fixed::F_FRAC) } else { "i".into() };
    }
    let src = ["", "f32:", "f64:"][(src_of(mode) as usize).min(2)];
    let r = ["", "g", "e", "py", "js", "pos"][(render_of(mode) as usize).min(5)];
    let p = if matches!(render_of(mode), dec::R_GEN | dec::R_SCI) {
        format!("{}", f & general::G_PREC)
    } else {
        String::new()
    };
    format!(
        "{src}{r}{p}{}{}",
        if f & general::G_EXP3 != 0 { "E" } else { "" },
        if f & general::G_NEGZ != 0 { "-" } else { "" }
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn xorshift(r: &mut u64) -> u64 {
        *r ^= *r << 13;
        *r ^= *r >> 7;
        *r ^= *r << 17;
        *r
    }

    /// Every descriptor `valid` accepts renders any value at any scale in range (what a
    /// decoder gets from corrupt input), and a token it renders codes back to the same
    /// decimal in the same mode, unless the value has more digits than the mode prints.
    #[test]
    fn every_descriptor() {
        let mut r = 0x9E3779B97F4A7C15u64;
        let mut tmp = vec![];
        let (mut n, mut back) = (0, 0);
        for mode in 0..=255u8 {
            for f in 0..=255u8 {
                if !valid(mode, f) {
                    continue;
                }
                n += 1;
                for i in 0..64 {
                    let x = xorshift(&mut r);
                    let v = match i % 4 {
                        0 => x as i64,
                        1 => (x as i64) >> (x % 60),
                        2 => [0, 1, -1, i64::MIN, i64::MAX][(x % 5) as usize],
                        _ => (x % 2000) as i64 - 1000,
                    };
                    let s = MIN_SCALE + (xorshift(&mut r) % (MAX_SCALE - MIN_SCALE + 1) as u64) as i32;
                    let mut out = vec![];
                    put(&mut out, v, s, mode, f);
                    assert!(!out.is_empty() && out.is_ascii(), "{mode:#x} {f:#x} {v} {s}");
                    if render_of(mode) == dec::R_FIX || src_of(mode) != dec::SRC_DEC || v == 0 {
                        continue;
                    }
                    let p = f & general::G_PREC;
                    let d = dec::Dec::from_scaled(v, s);
                    if render_of(mode) == dec::R_SCI && d.m.ilog10() >= p as u32 {
                        continue; // %e keeps the first P digits
                    }
                    let Some((v2, s2, f2)) = encode(&out, mode, p, f & general::G_EXP3 != 0, s, &mut tmp) else {
                        continue;
                    };
                    // Negative zero is meaningless for other values.
                    assert_eq!(f2, f & !general::G_NEGZ, "{}", String::from_utf8_lossy(&out));
                    assert_eq!(dec::Dec::from_scaled(v2, s2), d);
                    back += 1;
                }
            }
        }
        assert_eq!(n, 526);
        assert!(back > 4000, "{back} coded back");
    }

    /// Żmij's shortest digits are those of the standard library's (`{:e}` of a double), but
    /// for ties between two shortest decimals equally near the double: Żmij takes the even
    /// last digit (as ECMAScript requires and Python's `repr` does; checked against Python on
    /// 4M doubles), the standard library does not always (about 0.1% of these).
    #[test]
    fn shortest_matches_std() {
        let mut r = 0x2545F4914F6CDD1Du64;
        for i in 0..400000 {
            let x = xorshift(&mut r);
            let x = if i % 2 == 0 { f64::from_bits(x >> 1) } else { f32::from_bits((x >> 33) as u32) as f64 };
            if !x.is_finite() || x == 0.0 {
                continue;
            }
            let d = dec::parse_dec(format!("{x:e}").as_bytes()).unwrap();
            let mut b = zmij::Buffer::new();
            let z = dec::parse_dec(b.format_finite(x).as_bytes()).unwrap();
            if z != d {
                assert!(z.e == d.e && z.m.abs_diff(d.m) == 1 && z.m.is_multiple_of(2), "{x:e}");
            }
        }
    }

    #[test]
    fn descriptions() {
        assert_eq!(describe(M_FIX, 6 | fixed::F_POINT), "6");
        assert_eq!(describe(M_FIX, 0), "i");
        assert_eq!(describe(M_GEN, 17 | general::G_EXP3), "g17E");
        assert_eq!(describe(mode(dec::R_PY, dec::SRC_F32), general::G_NEGZ), "f32:py-");
    }
}
