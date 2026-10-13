//! C's `%.Pg` of decimals. The flags byte:
//!
//! ```text
//! bits 0-4  precision P (1..=17; 1..=19 for `%.Pe`, which shares these flags)
//! bit 5     exponents of at least three digits (old MSVC), else two
//! bit 6     negative zero
//! ```

use crate::fixed::{MAX_SCALE, MIN_SCALE, POW10};

pub const G_PREC: u8 = 31;
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
        while m.is_multiple_of(10) {
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
    while m.is_multiple_of(10) {
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

#[cfg(test)]
mod tests {
    use super::*;

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
            let x = f64::from_bits(r);
            let x = if x.is_finite() && i % 2 == 0 {
                x
            } else {
                ((r >> 11) as f64 / (1u64 << 53) as f64 - 0.5) * 10f64.powi((r % 20) as i32 - 8)
            };
            let x = if i % 97 == 0 {
                0.0
            } else if i % 101 == 0 {
                -0.0
            } else {
                x
            };
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
            let (v, s, f) =
                parse_gen(t, p, 0, false, &mut vec![]).unwrap_or_else(|| panic!("{}", String::from_utf8_lossy(t)));
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
