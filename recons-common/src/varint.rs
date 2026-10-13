//! LEB128 (7 bits per byte, least significant group first) with zigzag for signed values.

use crate::Corrupt;

#[inline]
pub fn zz(v: i64) -> u64 {
    ((v << 1) ^ (v >> 63)) as u64
}

#[inline]
pub fn unzz(v: u64) -> i64 {
    (v >> 1) as i64 ^ -((v & 1) as i64)
}

#[inline]
pub fn put(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push(v as u8 | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

#[inline]
pub fn put_signed(out: &mut Vec<u8>, v: i64) {
    put(out, zz(v))
}

/// Length of the encoding of `v`.
#[inline]
pub fn len(v: u64) -> usize {
    (64 - (v | 1).leading_zeros() as usize).div_ceil(7)
}

/// Reads a varint at `*at` and advances past it. At most ten bytes are read; bits beyond
/// 64 are dropped.
#[inline]
pub fn get(src: &[u8], at: &mut usize) -> Result<u64, Corrupt> {
    // Fast path: one byte.
    if let Some(&b) = src.get(*at)
        && b < 0x80
    {
        *at += 1;
        return Ok(b as u64);
    }
    get_slow(src, at)
}

#[cold]
fn get_slow(src: &[u8], at: &mut usize) -> Result<u64, Corrupt> {
    let mut v = 0u64;
    for shift in (0..64).step_by(7) {
        let b = *src.get(*at).ok_or(Corrupt("truncated"))?;
        *at += 1;
        v |= ((b & 0x7f) as u64) << shift;
        if b < 0x80 {
            return Ok(v);
        }
    }
    Err(Corrupt("bad varint"))
}

#[inline]
pub fn get_signed(src: &[u8], at: &mut usize) -> Result<i64, Corrupt> {
    get(src, at).map(unzz)
}

/// [`get`] that never fails: past the end it reads zeros (the caller checks the position
/// against the length afterwards).
#[inline]
pub fn get_lenient(src: &[u8], at: &mut usize) -> u64 {
    let mut v = 0u64;
    let mut s = 0;
    loop {
        let c = src.get(*at).copied().unwrap_or(0);
        *at += 1;
        v |= ((c & 0x7f) as u64) << s;
        if c < 0x80 || s >= 63 {
            return v;
        }
        s += 7;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let vals = [0u64, 1, 127, 128, 300, 16383, 16384, 1 << 35, u64::MAX - 1, u64::MAX];
        let mut b = Vec::new();
        for &v in &vals {
            put(&mut b, v);
            put_signed(&mut b, v as i64);
        }
        let (mut at, mut at2) = (0, 0);
        for &v in &vals {
            let p = at;
            assert_eq!(get(&b, &mut at), Ok(v));
            assert_eq!(at - p, len(v));
            assert_eq!(get_lenient(&b, &mut at2), v);
            assert_eq!(get_signed(&b, &mut at), Ok(v as i64));
            assert_eq!(unzz(get_lenient(&b, &mut at2)), v as i64);
        }
        assert_eq!(at, b.len());
        assert_eq!(at2, b.len());
        assert_eq!(get(&b, &mut at), Err(Corrupt("truncated")));
        assert_eq!(get(&[0x80; 11], &mut 0), Err(Corrupt("bad varint")));
        assert_eq!(get(&[0x80, 0x80], &mut 0), Err(Corrupt("truncated")));
    }
}
