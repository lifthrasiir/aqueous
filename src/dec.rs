//! Numbers printed from binary floats, and the ways writers print decimals.
//!
//! A number token is a decimal `D` (coded as an integer at a scale) plus a mode:
//!
//! - a source: the printed value is `D` itself, or `D` rounded to an `f32` or an `f64`;
//! - a render: how the value is printed. `%.Pg` and `%.Pe` (C), Python's `repr`,
//!   JavaScript's `Number.prototype.toString`, Rust's `Display` (positional, never an
//!   exponent). Fixed `%.Nf` is `num::put`, for decimal sources only.
//!
//! With a float source, `D` is the shortest decimal that rounds to the float, so a float
//! printed with more digits than it holds (`%.17g`, `%.18e`, the repr of a widened `f32`)
//! costs what the float holds. Shortest decimals come from Żmij; exact digits of a double
//! from 128-bit integer arithmetic (the standard library past its range). Conversions use
//! IEEE basic operations only, so they give the same results everywhere.

use crate::num::{self, G_EXP3, G_NEGZ, G_PREC};

pub const R_FIX: u8 = 0;
pub const R_GEN: u8 = 1;
pub const R_SCI: u8 = 2;
pub const R_PY: u8 = 3;
pub const R_JS: u8 = 4;
pub const R_POS: u8 = 5;
pub const SRC_DEC: u8 = 0;
pub const SRC_F32: u8 = 1;
pub const SRC_F64: u8 = 2;

#[inline]
pub fn mode(render: u8, src: u8) -> u8 {
    render | src << 4
}

#[inline]
pub fn render_of(mode: u8) -> u8 {
    mode & 15
}

#[inline]
pub fn src_of(mode: u8) -> u8 {
    mode >> 4
}

/// Whether a mode and flags (the second byte of a template item) can be rendered.
pub fn valid(mode: u8, f: u8) -> bool {
    let (r, s) = (render_of(mode), src_of(mode));
    match r {
        R_FIX => s == SRC_DEC && num::valid_fmt(f),
        R_GEN | R_SCI => s <= SRC_F64 && (1..=19).contains(&(f & G_PREC)) && f & 128 == 0 && (r == R_SCI || f & G_PREC <= 17),
        R_PY | R_JS | R_POS => s <= SRC_F64 && f & !G_NEGZ == 0,
        _ => false,
    }
}

const POW10: [u64; 20] = {
    let mut t = [1u64; 20];
    let mut i = 1;
    while i < 20 {
        t[i] = t[i - 1] * 10;
        i += 1;
    }
    t
};

/// `m` × 10^`e`, `m` without trailing zeros (0 for zero).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Dec {
    pub neg: bool,
    pub m: u64,
    pub e: i32,
}

#[inline]
fn ndigits(m: u64) -> i32 {
    m.checked_ilog10().unwrap_or(0) as i32 + 1
}

impl Dec {
    #[inline]
    pub fn new(neg: bool, mut m: u64, mut e: i32) -> Dec {
        if m == 0 {
            return Dec { neg, m: 0, e: 0 };
        }
        while m % 10 == 0 {
            m /= 10;
            e += 1;
        }
        Dec { neg, m, e }
    }

    #[inline]
    pub fn from_scaled(v: i64, s: i32) -> Dec {
        Dec::new(v < 0, v.unsigned_abs(), s.wrapping_neg())
    }

    /// The signed integer at scale `s`, if exact and in range.
    #[inline]
    pub fn scaled(&self, s: i32) -> Option<i64> {
        if self.m == 0 {
            return Some(0);
        }
        let k = self.e.checked_add(s)?;
        let v = self.m.checked_mul(*POW10.get(usize::try_from(k).ok()?)?)?;
        let v = i64::try_from(v).ok()?;
        Some(if self.neg { -v } else { v })
    }

    /// Decimal exponent of the first digit.
    #[inline]
    pub fn exp(&self) -> i32 {
        ndigits(self.m) - 1 + self.e
    }
}

/// Parses `[-]digits[.digits][(e|E)[+|-]digits]`: at least one digit, at most 19 significant
/// ones, an exponent of at most 4 digits.
#[inline]
pub fn parse_dec(t: &[u8]) -> Option<Dec> {
    // Fast path: at most 19 digits in all.
    let neg = t.first() == Some(&b'-');
    let mut i = neg as usize;
    let mut m = 0u64;
    let s = i;
    while i < t.len() && t[i].is_ascii_digit() {
        m = m.wrapping_mul(10).wrapping_add((t[i] - b'0') as u64);
        i += 1;
    }
    let mut nd = i - s;
    let mut e = 0i32;
    if i < t.len() && t[i] == b'.' {
        i += 1;
        let f = i;
        while i < t.len() && t[i].is_ascii_digit() {
            m = m.wrapping_mul(10).wrapping_add((t[i] - b'0') as u64);
            i += 1;
        }
        nd += i - f;
        e = -((i - f) as i32);
    }
    if nd == 0 || nd > 19 {
        return parse_dec_slow(t);
    }
    if i < t.len() {
        if !matches!(t[i], b'e' | b'E') {
            return None;
        }
        i += 1;
        let en = t.get(i) == Some(&b'-');
        i += matches!(t.get(i), Some(b'-' | b'+')) as usize;
        let d = &t[i..];
        if d.is_empty() || d.len() > 4 || !d.iter().all(u8::is_ascii_digit) {
            return None;
        }
        let x = d.iter().fold(0i32, |a, &c| a * 10 + (c - b'0') as i32);
        e += if en { -x } else { x };
    }
    Some(Dec::new(neg, m, e))
}

/// `parse_dec` for more than 19 digits (leading and trailing zeros not counted).
#[cold]
fn parse_dec_slow(t: &[u8]) -> Option<Dec> {
    let neg = t.first() == Some(&b'-');
    let mut i = neg as usize;
    let (mut m, mut nd, mut e) = (0u64, 0, 0i32);
    // Zeros after the last nonzero digit, not yet in m.
    let mut zeros = 0;
    let mut seen = false;
    let mut point = false;
    while let Some(&c) = t.get(i) {
        if c == b'.' && !point {
            point = true;
        } else if c.is_ascii_digit() {
            seen = true;
            if c == b'0' {
                zeros += (m != 0) as usize;
            } else {
                nd += zeros + 1;
                if nd > 19 {
                    return None;
                }
                m = m * POW10[zeros] * 10 + (c - b'0') as u64;
                zeros = 0;
            }
            e -= point as i32;
        } else {
            break;
        }
        i += 1;
    }
    if !seen {
        return None;
    }
    if i < t.len() {
        if !matches!(t[i], b'e' | b'E') {
            return None;
        }
        i += 1;
        let en = t.get(i) == Some(&b'-');
        i += matches!(t.get(i), Some(b'-' | b'+')) as usize;
        let d = &t[i..];
        if d.is_empty() || d.len() > 4 || !d.iter().all(u8::is_ascii_digit) {
            return None;
        }
        let x = d.iter().fold(0i32, |a, &c| a * 10 + (c - b'0') as i32);
        e += if en { -x } else { x };
    }
    Some(Dec::new(neg, m, e + zeros as i32))
}

/// Shortest decimal that rounds to `x` (finite).
#[inline]
fn shortest<F: zmij::Float>(x: F) -> Dec {
    let mut b = zmij::Buffer::new();
    parse_dec(b.format_finite(x).as_bytes()).expect("zmij output")
}

const F64_POW10: [f64; 23] = {
    let mut t = [1f64; 23];
    let mut i = 1;
    while i < 23 {
        t[i] = t[i - 1] * 10.0;
        i += 1;
    }
    t
};

/// `d` rounded to a double: one IEEE operation on exact operands when it can, else the
/// standard library's (correctly rounded) parser.
#[inline]
pub fn to_f64(d: Dec) -> f64 {
    let x = if d.m < 1 << 53 && (-22..=22).contains(&d.e) {
        if d.e >= 0 { d.m as f64 * F64_POW10[d.e as usize] } else { d.m as f64 / F64_POW10[-d.e as usize] }
    } else {
        format!("{}e{}", d.m, d.e).parse().unwrap_or(f64::INFINITY)
    };
    if d.neg { -x } else { x }
}

/// Exactly `p` (1..=19) significant digits of `x` (positive, finite), rounded half to even,
/// and the decimal exponent of the first one.
pub fn exact_digits(x: f64, p: u32) -> (u64, i32) {
    let bits = x.to_bits();
    let be = ((bits >> 52) & 0x7ff) as i32;
    let (mant, q) = if be == 0 { (bits & ((1 << 52) - 1), -1074) } else { ((bits & ((1 << 52) - 1)) | 1 << 52, be - 1075) };
    // log10(2) ~ 1233/4096; off by at most one, fixed below.
    let mut e10 = ((63 - mant.leading_zeros() as i32 + q) * 1233) >> 12;
    let lo = POW10[p as usize - 1];
    let hi = lo as u128 * 10;
    for _ in 0..3 {
        let Some(mut n) = scaled_round(mant, q, p as i32 - 1 - e10) else { break };
        if n >= hi {
            e10 += 1;
            continue;
        }
        if n < lo as u128 {
            e10 -= 1;
            continue;
        }
        // Rounding up can reach 10^p only from below it: the result is then 10^(p-1).
        if n == hi {
            n = lo as u128;
            e10 += 1;
        }
        return (n as u64, e10);
    }
    let s = format!("{:.*e}", p as usize - 1, x);
    let d = parse_dec(s.as_bytes()).expect("std exponent format");
    let e = d.exp();
    (d.m * POW10[(p as i32 - 1 - (e - d.e)) as usize], e)
}

/// `mant` × 2^`q` × 10^`k` rounded half to even, if the arithmetic fits 128 bits.
#[inline]
fn scaled_round(mant: u64, q: i32, k: i32) -> Option<u128> {
    // num / den with num = mant * 5^k+ * 2^(q+k)+, den = 5^k- * 2^(q+k)-
    let p5 = |n: i32| -> Option<u128> { if n <= 55 { Some(5u128.pow(n as u32)) } else { None } };
    let t = q + k;
    let mut num = mant as u128;
    let mut den = 1u128;
    if k >= 0 {
        num = num.checked_mul(p5(k)?)?;
    } else {
        den = p5(-k)?;
    }
    if t >= 0 {
        if t >= 128 || num.leading_zeros() <= t as u32 {
            return None;
        }
        num <<= t;
    } else {
        if -t >= 128 || den.leading_zeros() <= (-t) as u32 {
            return None;
        }
        if den == 1 {
            // A power of two: shifts, not a 128-bit division.
            let sh = -t as u32;
            let (n, r) = (num >> sh, num & ((1 << sh) - 1));
            let half = 1u128 << (sh - 1);
            return Some(if r > half || (r == half && n & 1 == 1) { n + 1 } else { n });
        }
        den <<= -t;
    }
    let (n, r) = (num / den, num % den);
    let half = den - r; // r > den/2 iff r > half, ties when r == half
    Some(if r > half || (r == half && n & 1 == 1) { n + 1 } else { n })
}

static PAIRS: [[u8; 2]; 100] = {
    let mut t = [[0u8; 2]; 100];
    let mut i = 0;
    while i < 100 {
        t[i] = [b'0' + (i / 10) as u8, b'0' + (i % 10) as u8];
        i += 1;
    }
    t
};

/// Digits of `m` into the end of `buf`; returns them.
#[inline]
pub fn digits(mut m: u64, buf: &mut [u8; 20]) -> &[u8] {
    let mut i = 20;
    while m >= 100 {
        i -= 2;
        buf[i..i + 2].copy_from_slice(&PAIRS[(m % 100) as usize]);
        m /= 100;
    }
    if m >= 10 {
        i -= 2;
        buf[i..i + 2].copy_from_slice(&PAIRS[m as usize]);
    } else {
        i -= 1;
        buf[i] = b'0' + m as u8;
    }
    &buf[i..]
}

fn put_exp(out: &mut Vec<u8>, e: i32, width: usize, plus: bool) {
    out.push(b'e');
    if e < 0 {
        out.push(b'-');
    } else if plus {
        out.push(b'+');
    }
    let mut b = [0u8; 20];
    let d = digits(e.unsigned_abs() as u64, &mut b);
    out.extend(std::iter::repeat_n(b'0', width.saturating_sub(d.len())));
    out.extend_from_slice(d);
}

/// Positional digits `d` with exponent `e` (of the first digit), no trailing zeros added
/// past the point; `int_point` adds ".0" to integers.
fn put_positional(out: &mut Vec<u8>, d: &[u8], e: i32, int_point: bool) {
    if e < 0 {
        out.extend_from_slice(b"0.");
        out.extend(std::iter::repeat_n(b'0', (-e - 1) as usize));
        out.extend_from_slice(d);
    } else if (e as usize) < d.len() - 1 {
        out.extend_from_slice(&d[..e as usize + 1]);
        out.push(b'.');
        out.extend_from_slice(&d[e as usize + 1..]);
    } else {
        out.extend_from_slice(d);
        out.extend(std::iter::repeat_n(b'0', e as usize + 1 - d.len()));
        if int_point {
            out.extend_from_slice(b".0");
        }
    }
}

/// Renders a value, given by its digits `m` (no trailing zeros needed but allowed for
/// `%e`; 0 for zero) and the exponent `e` of the first one, in render `r`.
fn put_rendered(out: &mut Vec<u8>, r: u8, f: u8, neg: bool, m: u64, e: i32) {
    let mut b = [0u8; 20];
    let d = digits(m, &mut b);
    let p = (f & G_PREC) as usize;
    let zero = m == 0;
    if neg && !(zero && r == R_JS) {
        out.push(b'-');
    }
    // Digits without trailing zeros (for all but %e).
    let mut ds = d;
    while ds.len() > 1 && ds[ds.len() - 1] == b'0' {
        ds = &ds[..ds.len() - 1];
    }
    let w = if f & G_EXP3 != 0 { 3 } else { 2 };
    match r {
        R_SCI => {
            out.push(d[0]);
            if p > 1 {
                out.push(b'.');
                let rest = &d[1..d.len().min(p)];
                out.extend_from_slice(rest);
                out.extend(std::iter::repeat_n(b'0', p - 1 - rest.len()));
            }
            put_exp(out, e, w, true);
        }
        R_GEN => {
            if !zero && (e < -4 || e >= p as i32) {
                out.push(ds[0]);
                if ds.len() > 1 {
                    out.push(b'.');
                    out.extend_from_slice(&ds[1..]);
                }
                put_exp(out, e, w, true);
            } else {
                put_positional(out, ds, e, false);
            }
        }
        R_PY => {
            if zero || (-4..16).contains(&e) {
                put_positional(out, ds, e, true);
            } else {
                out.push(ds[0]);
                if ds.len() > 1 {
                    out.push(b'.');
                    out.extend_from_slice(&ds[1..]);
                }
                put_exp(out, e, 2, true);
            }
        }
        R_JS => {
            if zero || (-6..21).contains(&e) {
                put_positional(out, ds, e, false);
            } else {
                out.push(ds[0]);
                if ds.len() > 1 {
                    out.push(b'.');
                    out.extend_from_slice(&ds[1..]);
                }
                put_exp(out, e, 1, true);
            }
        }
        _ => put_positional(out, ds, e, false),
    }
}

/// Appends the token for `v` at scale `s` in `mode` with flags `f` (a pair `valid`
/// accepts).
pub fn put(out: &mut Vec<u8>, v: i64, s: i32, mode: u8, f: u8) {
    let (r, src) = (render_of(mode), src_of(mode));
    if r == R_FIX {
        return num::put(out, v, f);
    }
    if r == R_GEN && src == SRC_DEC {
        return num::put_gen(out, v, s, f);
    }
    let d = Dec::from_scaled(v, s);
    let negz = f & G_NEGZ != 0;
    let p = (f & G_PREC) as u32;
    if src == SRC_DEC {
        let (m, e) = if r == R_SCI && d.m != 0 {
            // %e keeps P digits: pad with zeros (D never has more).
            let n = ndigits(d.m);
            let m = if n < p as i32 { d.m * POW10[(p as i32 - n).min(19) as usize] } else { d.m };
            (m, d.exp())
        } else {
            (d.m, d.exp())
        };
        return put_rendered(out, r, f, d.neg || (d.m == 0 && negz), m, e);
    }
    let mut x = to_f64(d);
    if src == SRC_F32 {
        x = x as f32 as f64;
    }
    if d.m == 0 && negz {
        x = -0.0;
    }
    if !x.is_finite() {
        // Only from corrupt streams: the encoder never codes such values.
        out.extend_from_slice(b"inf");
        return;
    }
    let neg = x.is_sign_negative();
    let a = x.abs();
    let (m, e) = if a == 0.0 {
        (0, 0)
    } else if r == R_GEN || r == R_SCI {
        exact_digits(a, p)
    } else {
        let d = shortest(a);
        (d.m, d.exp())
    };
    put_rendered(out, r, f, neg, m, e);
}

/// Codes token `t` in `mode` (not fixed) with precision `p` (for `%g`, `%e`): the integer,
/// its scale, and flags; None if the token is not printed that way. `prev_scale` is the
/// scale given to zero; `exp3` the exponent width for tokens without an exponent. `tmp` is
/// scratch space.
pub fn encode(t: &[u8], mode: u8, p: u8, exp3: bool, prev_scale: i32, tmp: &mut Vec<u8>) -> Option<(i64, i32, u8)> {
    let (r, src) = (render_of(mode), src_of(mode));
    if r == R_GEN && src == SRC_DEC {
        return num::parse_gen(t, p, prev_scale, exp3, tmp);
    }
    let ex = t.iter().position(|&c| c == b'e' || c == b'E');
    let ex3 = ex.map_or(exp3, |i| t.len() - i - 2 == 3 && matches!(t[i + 1], b'+' | b'-'));
    let mut f = match r {
        R_GEN | R_SCI => p | if ex3 { G_EXP3 } else { 0 },
        _ => 0,
    };
    let d = match src {
        SRC_DEC => parse_dec(t)?,
        _ => {
            if !t.iter().all(|&c| c.is_ascii_digit() || matches!(c, b'-' | b'+' | b'.' | b'e' | b'E')) {
                return None;
            }
            // SAFETY: ASCII, checked above.
            let s = unsafe { std::str::from_utf8_unchecked(t) };
            if src == SRC_F32 {
                let x: f32 = s.parse().ok()?;
                if !x.is_finite() {
                    return None;
                }
                let d = if x == 0.0 { Dec::new(false, 0, 0) } else { shortest(x.abs()) };
                Dec { neg: x.is_sign_negative(), ..d }
            } else {
                let x: f64 = s.parse().ok()?;
                if !x.is_finite() {
                    return None;
                }
                let d = if x == 0.0 { Dec::new(false, 0, 0) } else { shortest(x.abs()) };
                Dec { neg: x.is_sign_negative(), ..d }
            }
        }
    };
    if d.m == 0 && d.neg {
        f |= G_NEGZ;
    }
    let s = if d.m == 0 {
        prev_scale
    } else if r == R_SCI && src == SRC_DEC {
        p as i32 - 1 - d.exp()
    } else {
        -d.e
    };
    if !(num::MIN_SCALE..=num::MAX_SCALE).contains(&s) {
        return None;
    }
    let v = d.scaled(s)?;
    tmp.clear();
    put(tmp, v, s, mode, f);
    (tmp[..] == *t).then_some((v, s, f))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rt(t: &str, mode: u8, p: u8) {
        let (v, s, f) = encode(t.as_bytes(), mode, p, false, 0, &mut vec![]).unwrap_or_else(|| panic!("{t} mode {mode:#x} p {p}"));
        assert!(valid(mode, f), "{t}");
        let mut o = vec![];
        put(&mut o, v, s, mode, f);
        assert_eq!(std::str::from_utf8(&o).unwrap(), t);
    }

    #[test]
    fn writers() {
        let f32m = |r| mode(r, SRC_F32);
        let f64m = |r| mode(r, SRC_F64);
        for t in ["476.1831970214844", "128.5883026123047", "-0.10000000149011612", "1.0", "0.0", "-0.0", "9.999999747378752e-06", "1.2345678257474901e-08", "3.0000000054977558e+38", "0.5"] {
            rt(t, f32m(R_PY), 0);
        }
        for t in ["476.1831970214844", "1", "0", "0.000009999999747378752", "1.2345678257474901e-8", "1.0000000116860974e-7", "3.3999999521443642e+38", "123456792"] {
            rt(t, f32m(R_JS), 0);
        }
        for t in ["4.761831999999999994e+02", "-1.285883000000000038e+02", "0.000000000000000000e+00"] {
            rt(t, f64m(R_SCI), 19);
        }
        for t in ["4.761832e+02", "-1.000000e-05", "0.000000e+00"] {
            rt(t, mode(R_SCI, SRC_DEC), 7);
            rt(t, f32m(R_SCI), 7);
        }
        for t in ["12.759518999999999", "-2.470218", "39.257838999999997", "0", "1.0000000000000001e-05", "0.10000000000000001"] {
            rt(t, f64m(R_GEN), 17);
        }
        for t in ["-0.00202200002968311", "-0.0401785001158714"] {
            rt(t, f32m(R_GEN), 15);
        }
        for t in ["476.183197", "128.588303", "9.99999975e-06", "-0.100000001"] {
            rt(t, f32m(R_GEN), 9);
        }
        for t in ["476.1832", "0.0000001", "1", "-0", "100000000000000000000"] {
            rt(t, mode(R_POS, SRC_DEC), 0);
        }
        for t in ["0.1", "1e-05", "1.0", "1e+16", "123.456", "-0.0"] {
            rt(t, mode(R_PY, SRC_DEC), 0);
        }
        assert!(encode(b"0.1", f32m(R_PY), 0, false, 0, &mut vec![]).is_none());
    }

    #[test]
    fn exact_digits_match_std() {
        let mut r = 0x9E3779B97F4A7C15u64;
        for i in 0..200000 {
            r ^= r << 13;
            r ^= r >> 7;
            r ^= r << 17;
            let x = f64::from_bits(r >> 1);
            if !x.is_finite() || x == 0.0 {
                continue;
            }
            let p = 1 + (i % 19) as u32;
            let (m, e) = exact_digits(x, p);
            let s = format!("{:.*e}", p as usize - 1, x);
            let d = parse_dec(s.as_bytes()).unwrap();
            assert_eq!(e, d.exp(), "{x:e} p {p}");
            assert_eq!(Dec::new(false, m, e + 1 - p as i32), Dec { neg: false, ..d }, "{x:e} p {p}");
        }
    }
}
