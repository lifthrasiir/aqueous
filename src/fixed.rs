//! Fixed tokens (`%.Nf`-like), and scales.
//!
//! A token `[-][int][.frac]` is its digits as one integer (signed) and a format byte:
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
            "0",
            "-0",
            "0.5",
            ".5",
            "-.5",
            "-0.000000",
            "1.000000",
            "123.456",
            "5.",
            "-5.",
            "0.",
            "100",
            "999999999999999999",
            "-0.00000000000000001",
            ".000000000000000001",
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
}
