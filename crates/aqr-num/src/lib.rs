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
//! - [`time`]: times printed by a pattern, as one integer.
//! - [`times`]: the encoder's finding of times in a line or a field.
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
pub mod time;
pub mod times;

pub use dec::{COMMA, GROUP, comma_of, encode, group_of, is_fixed, mode, put, render_of, src_of, valid};
pub use fixed::{MAX_SCALE, MIN_SCALE};

/// Fixed (`%.Nf`-like) numbers: their scale is in the flags.
pub const M_FIX: u8 = dec::mode(dec::R_FIX, dec::SRC_DEC);
/// C's `%.Pg` of decimals.
pub const M_GEN: u8 = dec::mode(dec::R_GEN, dec::SRC_DEC);

/// A readable form of a descriptor (one `valid` accepts), for analysis: fixed numbers by
/// their digits after the point ("i" for none, "#" without an integer part, "-" for a
/// negative zero), others as `[f32:|f64:]render[P][E][-]` ("E" for three-digit exponents).
pub fn describe(mode: u8, f: u8) -> String {
    if is_fixed(mode) || render_of(mode) == dec::R_FIXP {
        let mut s = String::from(if comma_of(mode) { "," } else { "" });
        if !is_fixed(mode) {
            s += "p";
        }
        if f & fixed::F_NEGZ != 0 {
            s += "-";
        }
        if f & fixed::F_NOINT != 0 {
            s += "#";
        }
        return s + &if f & fixed::F_POINT != 0 { format!("{}", f & fixed::F_FRAC) } else { "i".into() };
    }
    let src = ["", "f32:", "f64:"][(src_of(mode) as usize).min(2)];
    let r = ["", "g", "e", "py", "js", "pos", "fixp"][(render_of(mode) as usize).min(6)];
    let p = if matches!(render_of(mode), dec::R_GEN | dec::R_SCI) {
        format!("{}", f & general::G_PREC)
    } else {
        String::new()
    };
    format!(
        "{}{src}{r}{p}{}{}",
        if comma_of(mode) { "," } else { "" },
        if f & general::G_EXP3 != 0 { "E" } else { "" },
        if f & general::G_NEGZ != 0 { "-" } else { "" }
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dec::{SRC_DEC, SRC_F64};

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
                    if render_of(mode) == dec::R_FIXP && s > (f & fixed::F_FRAC) as i32 {
                        continue; // more digits than the format's: another format codes them
                    }
                    let d = dec::Dec::from_scaled(v, s);
                    if render_of(mode) == dec::R_SCI && d.m.ilog10() >= p as u32 {
                        continue; // %e keeps the first P digits
                    }
                    let Some((v2, s2, f2)) = encode(&out, mode, p, f & general::G_EXP3 != 0, s, &mut tmp) else {
                        continue;
                    };
                    // Negative zero is meaningless for other values.
                    // (padded fixed: nor does "no integer part" tell where there is one, or a negative zero a number)
                    let (f2, want) = match render_of(mode) {
                        dec::R_FIXP => (f2 & !(fixed::F_NOINT | fixed::F_NEGZ), f & !(fixed::F_NOINT | fixed::F_NEGZ)),
                        _ => (f2, f & !general::G_NEGZ),
                    };
                    assert_eq!(f2, want, "{mode:#x} {f:#x} v {v} s {s} {}", String::from_utf8_lossy(&out));
                    assert_eq!(dec::Dec::from_scaled(v2, s2), d);
                    back += 1;
                }
            }
        }
        assert_eq!(n, 1520);
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

    /// A decimal comma (mode bit 6) prints the point as `,` and nothing else changes; a
    /// token with a point is not coded in a comma mode.
    #[test]
    fn decimal_comma() {
        let mut tmp = vec![];
        let comma = |m: u8| m | 0x40;
        for (t, m, p) in [
            ("1,5e-07", mode(dec::R_SCI, dec::SRC_DEC), 2),
            ("12,759518999999999", mode(dec::R_GEN, dec::SRC_F64), 17),
            ("0,1", mode(dec::R_PY, dec::SRC_DEC), 0),
            ("1,0", mode(dec::R_PY, dec::SRC_F32), 0),
            ("4,216", mode(dec::R_POS, dec::SRC_DEC), 0),
            ("1234", mode(dec::R_POS, dec::SRC_DEC), 0),
        ] {
            let (v, s, f) = encode(t.as_bytes(), comma(m), p, false, 0, &mut tmp).unwrap_or_else(|| panic!("{t}"));
            assert!(valid(comma(m), f), "{t}");
            let mut o = vec![];
            put(&mut o, v, s, comma(m), f);
            assert_eq!(std::str::from_utf8(&o).unwrap(), t);
            let mut o = vec![];
            put(&mut o, v, s, m, f);
            assert_eq!(std::str::from_utf8(&o).unwrap(), t.replace(',', "."));
            if t.contains(',') {
                assert!(encode(t.replace(',', ".").as_bytes(), comma(m), p, false, 0, &mut tmp).is_none(), "{t}");
                assert!(encode(t.as_bytes(), m, p, false, 0, &mut tmp).is_none(), "{t}");
            }
        }
        // fixed
        let (v, f) = fixed::parse(b"-0.05").unwrap();
        let mut o = vec![];
        put(&mut o, v, 2, M_FIX | 0x40, f);
        assert_eq!(o, b"-0,05");
        assert!(valid(M_FIX | 0x40, f));
        assert!(!valid(0x30 | 0x40, 0) && !valid(0x30, 0));
        // (a group's bit is valid for the fixed and positional renders only)
        assert!(valid(0x80, 0) && valid(0xC0, 0) && !valid(0x81, 3) && !valid(0xC3, 0));
        assert!(encode(b"1,2,3", comma(mode(dec::R_POS, dec::SRC_DEC)), 0, false, 0, &mut tmp).is_none());
    }

    /// Padded fixed (`%.Nf` of a decimal with the scale of its last digit): the integer is that
    /// of the digits without the zeros the format pads.
    #[test]
    fn padded_fixed() {
        let mut tmp = vec![];
        let m = mode(dec::R_FIXP, dec::SRC_DEC);
        for (t, v, s) in [
            ("42283.58000000", 4228358, 2),
            ("0.00012000", 12, 5),
            ("-0.00000000", 0, 3),
            ("1500.0", 1500, 0),
            ("5", 5, 0),
            ("0.5", 5, 1),
            ("-.250", -25, 2),
            ("0.000", 0, 3),
            ("12.", 12, 0),
        ] {
            for c in [0, 0x40] {
                let tok = if c != 0 { t.replace('.', ",") } else { t.to_string() };
                let (v2, s2, f) = encode(tok.as_bytes(), m | c, 0, false, 3, &mut tmp).unwrap_or_else(|| panic!("{t}"));
                assert_eq!((v2, s2), (v, if v == 0 { 3.min((f & fixed::F_FRAC) as i32) } else { s }), "{t}");
                assert!(valid(m | c, f), "{t}");
                let mut o = vec![];
                put(&mut o, v2, s2, m | c, f);
                assert_eq!(std::str::from_utf8(&o).unwrap(), tok);
            }
        }
        // (a scale past the format's digits prints them all; zeros are appended to others)
        let mut o = vec![];
        put(&mut o, 1234, 3, m, 2 | fixed::F_POINT);
        assert_eq!(o, b"1.234");
        o.clear();
        put(&mut o, 12, -3, m, 1 | fixed::F_POINT);
        assert_eq!(o, b"12000.0");
        // not of other tokens
        for t in ["1e5", "01.5", "+1", "1.2.3", "nan"] {
            assert!(encode(t.as_bytes(), m, 0, false, 0, &mut tmp).is_none(), "{t}");
        }
        // sources other than decimal are invalid
        assert!(!valid(mode(dec::R_FIXP, dec::SRC_F64), 0));
    }

    /// Digits grouped by threes (mode bit 7): the separator is a comma, or a point with the decimal
    /// comma, the integer part only; a token is coded in a mode with groups where it has them as it
    /// would print them, and a style of groups codes the tokens that are not grouped in the style
    /// without.
    #[test]
    fn digit_groups() {
        let mut tmp = vec![];
        let fix = |n: u8| n | fixed::F_POINT;
        for (mode, f, v, s, want) in [
            (M_FIX | GROUP, fix(2), 123456789, 2, "1,234,567.89"),
            (M_FIX | GROUP, fix(2), -100000, 2, "-1,000.00"),
            (M_FIX | GROUP, fix(2), 99999, 2, "999.99"),
            (M_FIX | GROUP, 0, 1000, 0, "1,000"),
            (M_FIX | GROUP, 0, 999, 0, "999"),
            (M_FIX | GROUP | COMMA, fix(2), 118725, 2, "1.187,25"),
            (M_FIX | GROUP | COMMA, fix(2), -123456789012, 2, "-1.234.567.890,12"),
            (M_FIX | GROUP | COMMA, fix(1) | fixed::F_NOINT, 5, 1, ",5"),
            (mode(dec::R_FIXP, SRC_DEC) | GROUP, fix(3), 4228358, 2, "42,283.580"),
            (mode(dec::R_POS, SRC_DEC) | GROUP, 0, 1234567, 3, "1,234.567"),
            (mode(dec::R_POS, SRC_F64) | GROUP | COMMA, 0, 12345678, -2, "1.234.567.800"),
        ] {
            assert!(valid(mode, f), "{want}");
            let mut o = vec![];
            put(&mut o, v, s, mode, f);
            assert_eq!(std::str::from_utf8(&o).unwrap(), want);
            let st = style::Style { mode, p: 0, exp3: false };
            let (v2, s2, m2, f2) = st.encode(want.as_bytes(), s, &mut tmp).unwrap_or_else(|| panic!("{want}"));
            assert_eq!(m2, mode, "{want}");
            let mut o = vec![];
            put(&mut o, v2, s2, m2, f2);
            assert_eq!(std::str::from_utf8(&o).unwrap(), want);
            // (ungrouped, it is not printed as the mode does)
            let sep = if mode & COMMA != 0 { '.' } else { ',' };
            let plain = want.replace(sep, "");
            if plain.len() > want.len() - want.matches(sep).count() || want.contains(sep) {
                // (the style codes it, in another mode)
                let (_, _, m3, _) = st.encode(plain.as_bytes(), s, &mut tmp).unwrap_or((0, 0, 0, 0));
                assert_ne!(m3, mode, "{plain}");
            }
        }
        // groups that are not as put prints them
        for (mode, t) in [
            (M_FIX | GROUP, "1234.5"),
            (M_FIX | GROUP, "1,23.4"),
            (M_FIX | GROUP, "12,34"),
            (M_FIX | GROUP, "1,2345.0"),
            (M_FIX | GROUP, ",234.5"),
            (M_FIX | GROUP, "1,234,.5"),
            (M_FIX | GROUP, "1.187,25"),
            (M_FIX | GROUP | COMMA, "1,187.25"),
            (M_FIX | GROUP | COMMA, "1.18,25"),
            (M_FIX | GROUP | COMMA, "1.187.25"),
            (M_FIX | GROUP | COMMA, "1.1875,2"),
        ] {
            assert!(encode(t.as_bytes(), mode, 0, false, 0, &mut tmp).is_none(), "{t}");
        }
        // the style of a sample
        let pick = |toks: &[&str]| {
            let toks: Vec<(usize, &[u8])> = toks.iter().map(|t| (0, t.as_bytes())).collect();
            style::choose(&toks)
        };
        let de = pick(&["1.187,25", "12,5", "-3.456.789,00", "999,99", "0,5"]);
        assert_eq!(de.mode & (COMMA | GROUP), COMMA | GROUP);
        let en = pick(&["1,234.56", "12.5", "-3,456,789.00", "999.99", "0.5"]);
        assert_eq!(en.mode & (COMMA | GROUP), GROUP);
        for (st, toks) in [
            (de, ["1.187,25", "12,5", "999,00", "1187,25", "-1.000,00"]),
            (en, ["1,187.25", "12.5", "999.00", "1187.25", "-1,000.00"]),
        ] {
            for t in toks {
                let (v, s, m, f) = st.encode(t.as_bytes(), 0, &mut tmp).unwrap_or_else(|| panic!("{t}"));
                let mut o = vec![];
                put(&mut o, v, s, m, f);
                assert_eq!(std::str::from_utf8(&o).unwrap(), t);
            }
        }
        assert_eq!(de.encode(b"1187,25", 0, &mut tmp).unwrap().2 & GROUP, 0);
        assert_eq!(de.encode(b"999,00", 0, &mut tmp).unwrap().2 & GROUP, GROUP);
        // one separator and no decimal one is a decimal point or comma; four digits and none are flat
        assert_eq!(pick(&["1.187", "2.345", "12,5"]).mode & GROUP, 0);
        // (IP addresses and versions are not integers: no decimal after the groups)
        assert_eq!(pick(&["173.234.131.186", "10.123.456.789"]).mode & GROUP, 0);
        assert_eq!(pick(&["1.187.234", "2,345,678", "12.5"]).mode & GROUP, 0);
        assert_eq!(pick(&["1,234", "5,678"]).mode & GROUP, 0);
        assert_eq!(pick(&["1.187,25", "1234,5", "5678,9"]).mode & GROUP, 0);
        assert_eq!(pick(&["1.5", "2.25", "1234.5"]).mode & GROUP, 0);
    }

    #[test]
    fn descriptions() {
        assert_eq!(describe(M_FIX, 6 | fixed::F_POINT), "6");
        assert_eq!(describe(M_FIX, 0), "i");
        assert_eq!(describe(M_GEN, 17 | general::G_EXP3), "g17E");
        assert_eq!(describe(mode(dec::R_PY, dec::SRC_F32), general::G_NEGZ), "f32:py-");
    }
}
