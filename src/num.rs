//! Decimal tokens as integers and formats.
//!
//! A number `[-][int][.frac]` is its digits as one integer (signed) and a format byte:
//!
//! ```text
//! bits 0-4  digits after the point (0..=18)
//! bit 5     point present
//! bit 6     integer part omitted (".5")
//! bit 7     negative zero ("-0.000")
//! ```
//!
//! Only tokens that format back to themselves are accepted: no leading zeros (but a lone
//! "0"), at most 18 digits, no '+' or exponent.

pub const F_FRAC: u8 = 31;
pub const F_POINT: u8 = 32;
pub const F_NOINT: u8 = 64;
pub const F_NEGZ: u8 = 128;

/// Whether a format byte is one `parse` can return.
pub fn valid_fmt(f: u8) -> bool {
    let frac = f & F_FRAC;
    frac <= 18 && (f & F_POINT != 0 || (frac == 0 && f & F_NOINT == 0)) && (f & F_NOINT == 0 || frac > 0)
}

#[inline]
pub fn parse(t: &[u8]) -> Option<(i64, u8)> {
    let mut i = 0;
    let neg = t.first() == Some(&b'-');
    i += neg as usize;
    let s = i;
    let mut m = 0i64;
    while let Some(&c) = t.get(i) {
        let d = c.wrapping_sub(b'0');
        if d > 9 {
            break;
        }
        m = m.wrapping_mul(10).wrapping_add(d as i64);
        i += 1;
    }
    let nint = i - s;
    if nint > 1 && t[s] == b'0' {
        return None;
    }
    let mut f = 0u8;
    let mut frac = 0usize;
    if t.get(i) == Some(&b'.') {
        f |= F_POINT;
        i += 1;
        let s = i;
        while let Some(&c) = t.get(i) {
            let d = c.wrapping_sub(b'0');
            if d > 9 {
                break;
            }
            m = m.wrapping_mul(10).wrapping_add(d as i64);
            i += 1;
        }
        frac = i - s;
    }
    if i != t.len() || nint + frac == 0 || nint + frac > 18 {
        return None;
    }
    f |= frac as u8;
    if nint == 0 {
        f |= F_NOINT;
    }
    if neg && m == 0 {
        f |= F_NEGZ;
    }
    Some((if neg { -m } else { m }, f))
}

/// Appends the token for `m` in format `f` (one `valid_fmt` accepts).
#[inline]
pub fn put(out: &mut Vec<u8>, m: i64, f: u8) {
    let mut buf = [b'0'; 24];
    let mut i = 24;
    let mut a = m.unsigned_abs();
    loop {
        i -= 1;
        buf[i] = b'0' + (a % 10) as u8;
        a /= 10;
        if a == 0 {
            break;
        }
    }
    let frac = (f & F_FRAC) as usize;
    let min = if f & F_NOINT != 0 { frac } else { frac + 1 };
    i = i.min(24 - min);
    if m < 0 || (m == 0 && f & F_NEGZ != 0) {
        out.push(b'-');
    }
    out.extend_from_slice(&buf[i..24 - frac]);
    if f & F_POINT != 0 {
        out.push(b'.');
        out.extend_from_slice(&buf[24 - frac..]);
    }
}

/// Scales (digits after the point) are kept in this range; outside it a token is not coded.
pub const MIN_SCALE: i32 = -60;
pub const MAX_SCALE: i32 = 60;

/// `%.Pg` flags: the precision is bits 0-4 (1..=17).
pub const G_PREC: u8 = 31;
/// Exponents of at least three digits (old MSVC), else two.
pub const G_EXP3: u8 = 32;
pub const G_NEGZ: u8 = 64;

pub fn valid_gen(f: u8) -> bool {
    (1..=17).contains(&(f & G_PREC)) && f & 128 == 0
}

/// Parses `t` as C's `%.Pg` of a decimal: the value as an integer at scale P-1-E (E the
/// decimal exponent) and that scale. `prev_scale` is the scale given to zero; `exp3`
/// the exponent width when `t` has no exponent; `tmp` scratch space. Returns the integer,
/// scale and flags.
pub fn parse_gen(t: &[u8], p: u8, prev_scale: i32, exp3: bool, tmp: &mut Vec<u8>) -> Option<(i64, i32, u8)> {
    let neg = t.first() == Some(&b'-');
    let mut i = neg as usize;
    let mut m = 0u64;
    let mut nd = 0u32; // significant digits taken into m
    let mut e10 = 0i32; // value = m * 10^e10
    let mut seen = false;
    let mut point = false;
    while let Some(&c) = t.get(i) {
        if c == b'.' && !point {
            point = true;
        } else if c.is_ascii_digit() {
            seen = true;
            if m != 0 || c != b'0' {
                if nd >= 18 {
                    return None;
                }
                m = m * 10 + (c - b'0') as u64;
                nd += 1;
            }
            if point {
                e10 -= 1;
            }
        } else {
            break;
        }
        i += 1;
    }
    if !seen {
        return None;
    }
    let mut ex3 = exp3;
    if i < t.len() {
        if t[i] != b'e' {
            return None;
        }
        let (es, ed) = (t.get(i + 1)?, &t[i + 2..]);
        if !matches!(es, b'+' | b'-') || !(2..=3).contains(&ed.len()) || !ed.iter().all(u8::is_ascii_digit) {
            return None;
        }
        let x = ed.iter().fold(0i32, |a, &c| a * 10 + (c - b'0') as i32);
        e10 += if *es == b'-' { -x } else { x };
        ex3 = ed.len() == 3;
    }
    let f = p | if ex3 { G_EXP3 } else { 0 } | if neg && m == 0 { G_NEGZ } else { 0 };
    let (v, s) = if m == 0 {
        (0, prev_scale)
    } else {
        while m % 10 == 0 {
            m /= 10;
            e10 += 1;
        }
        let sig = ndigits(m) as i32;
        if sig > p as i32 {
            return None;
        }
        // p-1-E, with E = sig-1+e10 the decimal exponent
        let s = p as i32 - sig - e10;
        if !(MIN_SCALE..=MAX_SCALE).contains(&s) {
            return None;
        }
        let v = m as i64 * POW10[(s + e10) as usize];
        (if neg { -v } else { v }, s)
    };
    tmp.clear();
    put_gen(tmp, v, s, f);
    (tmp[..] == *t).then_some((v, s, f))
}

#[inline]
fn ndigits(m: u64) -> u32 {
    m.checked_ilog10().unwrap_or(0) + 1
}

/// Appends C's `%.Pg` of `v` * 10^-`s` (flags `f`, one `valid_gen` accepts).
pub fn put_gen(out: &mut Vec<u8>, v: i64, s: i32, f: u8) {
    if v < 0 || (v == 0 && f & G_NEGZ != 0) {
        out.push(b'-');
    }
    let mut m = v.unsigned_abs();
    if m == 0 {
        out.push(b'0');
        return;
    }
    let mut s = s;
    while m % 10 == 0 {
        m /= 10;
        s -= 1;
    }
    let mut buf = [0u8; 20];
    let mut n = 0;
    while m > 0 {
        buf[19 - n] = b'0' + (m % 10) as u8;
        m /= 10;
        n += 1;
    }
    let d = &buf[20 - n..];
    let sig = n as i32;
    let e = sig - 1 - s;
    let p = (f & G_PREC) as i32;
    if e < -4 || e >= p {
        out.push(d[0]);
        if sig > 1 {
            out.push(b'.');
            out.extend_from_slice(&d[1..]);
        }
        out.push(b'e');
        out.push(if e < 0 { b'-' } else { b'+' });
        let a = e.unsigned_abs();
        let w = if f & G_EXP3 != 0 { 3 } else { 2 };
        let mut eb = [b'0'; 10];
        let mut k = 10;
        let mut x = a;
        while x > 0 || 10 - k < w {
            k -= 1;
            eb[k] = b'0' + (x % 10) as u8;
            x /= 10;
        }
        out.extend_from_slice(&eb[k..]);
    } else if s <= 0 {
        out.extend_from_slice(d);
        out.extend(std::iter::repeat_n(b'0', (-s) as usize));
    } else if e >= 0 {
        let ip = (e + 1) as usize;
        out.extend_from_slice(&d[..ip]);
        out.push(b'.');
        out.extend_from_slice(&d[ip..]);
    } else {
        out.extend_from_slice(b"0.");
        out.extend(std::iter::repeat_n(b'0', (-e - 1) as usize));
        out.extend_from_slice(d);
    }
}

pub const POW10: [i64; 19] = {
    let mut t = [1i64; 19];
    let mut i = 1;
    while i < 19 {
        t[i] = t[i - 1] * 10;
        i += 1;
    }
    t
};

/// `m` with `from` digits after the point, rescaled to `to` digits (truncating).
#[inline]
pub fn rescale(m: i64, from: i32, to: i32) -> i64 {
    if to >= from {
        m.wrapping_mul(POW10[((to - from) as usize).min(18)])
    } else {
        m / POW10[((from - to) as usize).min(18)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        for t in [
            "0", "-0", "0.5", ".5", "-.5", "-0.000000", "1.000000", "123.456", "5.", "-5.", "0.", "100",
            "999999999999999999", "-0.00000000000000001", ".000000000000000001",
        ] {
            let (m, f) = parse(t.as_bytes()).unwrap_or_else(|| panic!("{t}"));
            assert!(valid_fmt(f), "{t}");
            let mut o = vec![];
            put(&mut o, m, f);
            assert_eq!(std::str::from_utf8(&o).unwrap(), t);
        }
        for t in ["", "-", ".", "-.", "00", "01.5", "+1", "1e5", "1.2.3", "1..", "1234567890123456789", "nan"] {
            assert!(parse(t.as_bytes()).is_none(), "{t}");
        }
    }

    /// Checks against the C library's `%.*g`.
    #[test]
    fn gen_matches_libc() {
        unsafe extern "C" {
            fn snprintf(buf: *mut u8, n: usize, fmt: *const u8, ...) -> i32;
        }
        let mut r = 0x2545F4914F6CDD1Du64;
        for i in 0..200000 {
            r ^= r << 13;
            r ^= r >> 7;
            r ^= r << 17;
            let x = f64::from_bits(r) ;
            let x = if x.is_finite() && i % 2 == 0 { x } else { ((r >> 11) as f64 / (1u64 << 53) as f64 - 0.5) * 10f64.powi((r % 20) as i32 - 8) };
            let x = if i % 97 == 0 { 0.0 } else if i % 101 == 0 { -0.0 } else { x };
            if x != 0.0 && x.abs().log10().abs() > 30.0 {
                continue;
            }
            let p = 1 + (i % 17) as u8;
            let mut b = [0u8; 64];
            let n = unsafe { snprintf(b.as_mut_ptr(), 64, c"%.*g".as_ptr() as *const u8, p as i32, x) } as usize;
            let t = &b[..n];
            if t.len() > 24 {
                continue;
            }
            let (v, s, f) = parse_gen(t, p, 0, false, &mut vec![]).unwrap_or_else(|| panic!("{}", String::from_utf8_lossy(t)));
            assert!(valid_gen(f));
            let mut o = vec![];
            put_gen(&mut o, v, s, f);
            assert_eq!(o, t);
        }
        for t in ["1.64901e-008", "-0", "0", "1e+50", "123456", "0.0001", "1e-05"] {
            let p = 6;
            assert!(parse_gen(t.as_bytes(), p, 0, false, &mut vec![]).is_some(), "{t}");
        }
        for t in ["1.0", "0.10", "1234567", "1e5", "1e-5", "1.5e+02", "00.1"] {
            assert!(parse_gen(t.as_bytes(), 6, 0, false, &mut vec![]).is_none(), "{t}");
        }
    }
}
