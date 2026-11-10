//! The entropy coding of a slice's component: its blocks' DC coefficients, differences coded
//! one after another, then the AC coefficients as runs and levels, interleaved across the
//! blocks (scan position by scan position). Codewords are Rice codes that switch to
//! exp-Golomb codes, their parameters picked by the previous value. Given the coefficients
//! the bits are unique, but for the zero padding at the end.
//!
//! Coefficients are kept by block, in scan order: `c[b * 64 + i]` is block `b`'s `i`th in
//! scan order (`c[b * 64]` its DC, as coded: without the 4096 offset).

use std::hint::select_unpredictable;

use aqr_common::msb::BitReader;

use crate::{Error, Result};

const FIRST_DC_CB: u8 = 0xB8;
const DC_CB: [u8; 7] = [0x04, 0x28, 0x28, 0x4D, 0x4D, 0x70, 0x70];
const RUN_CB: [u8; 16] =
    [0x06, 0x06, 0x05, 0x05, 0x04, 0x29, 0x29, 0x29, 0x29, 0x28, 0x28, 0x28, 0x28, 0x28, 0x28, 0x4C];
const LEV_CB: [u8; 10] = [0x04, 0x0A, 0x05, 0x06, 0x04, 0x28, 0x28, 0x28, 0x28, 0x4C];

fn bad(what: &'static str) -> Error {
    Error(what)
}

/// A codeword of codebook `cb`: bits 5..7 the Rice order, 2..4 the exp-Golomb order, 0..1 the
/// largest unary prefix of a Rice code; longer prefixes escape to exp-Golomb.
#[inline(always)]
fn get_code(r: &mut BitReader, cb: u8) -> Result<u32> {
    let s = (cb & 3) as u32;
    let rk = (cb >> 5) as u32;
    let ek = ((cb >> 2) & 7) as u32;
    let w = r.window();
    let q = w.leading_zeros();
    if q > s {
        let bits = ek - s + 2 * q;
        if bits > 32 {
            return Err(bad("codeword too long"));
        }
        r.skip(bits);
        Ok(((w >> (64 - bits)) as u32) - (1 << ek) + ((s + 1) << rk))
    } else {
        r.skip(q + 1);
        Ok((q << rk) + r.get(rk))
    }
}

/// The codeword of `v` in codebook `cb` (see [`get_code`]), right aligned, and its length.
#[inline(always)]
fn codeword(cb: u8, v: u32) -> (u64, u32) {
    let s = (cb & 3) as u32;
    let rk = (cb >> 5) as u32;
    let ek = ((cb >> 2) & 7) as u32;
    if v < (s + 1) << rk {
        (((1 << rk) | (v & ((1 << rk) - 1))) as u64, (v >> rk) + 1 + rk)
    } else {
        let y = (v - ((s + 1) << rk)) as u64 + (1 << ek);
        let n = 64 - y.leading_zeros();
        (y, 2 * n - ek + s)
    }
}

/// MSB-first bits appended to a byte vector.
struct Out<'a> {
    out: &'a mut Vec<u8>,
    acc: u64,
    n: u32,
}

impl Out<'_> {
    /// The low `len` bits of `v` (`v` has no others set).
    #[inline(always)]
    fn put(&mut self, v: u64, len: u32) {
        if len > 32 {
            self.put_long(v, len);
            return;
        }
        self.acc = (self.acc << len) | v;
        self.n += len;
        if self.n >= 32 {
            self.n -= 32;
            self.out.extend_from_slice(&((self.acc >> self.n) as u32).to_be_bytes());
        }
    }

    #[cold]
    #[inline(never)]
    fn put_long(&mut self, v: u64, len: u32) {
        self.put(v >> 32, len - 32);
        self.put(v & 0xffff_ffff, 32);
    }

    /// Pads with zero bits to a byte.
    fn finish(mut self) {
        let pad = (8 - self.n % 8) % 8;
        self.put(0, pad);
        while self.n > 0 {
            self.n -= 8;
            self.out.push((self.acc >> self.n) as u8);
        }
    }
}

/// What a component's bits held besides its coefficients.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Tail {
    /// Whole zero bytes after the byte of the last codeword.
    pub zero_bytes: u32,
    /// No bytes at all (gray pictures' chroma): the coefficients are zero.
    pub empty: bool,
}

/// Decodes `blocks` (a power of two) blocks' coefficients from `d` (padded, see
/// [`aqr_common::msb::padded`]) bits `start..end` into `c`.
pub fn decode(d: &[u8], start: usize, end: usize, blocks: usize, c: &mut [i16]) -> Result<Tail> {
    debug_assert!(blocks.is_power_of_two() && c.len() == blocks * 64);
    c.fill(0);
    if start == end {
        return Ok(Tail { zero_bytes: 0, empty: true });
    }
    let mut r = BitReader::new(d, start);
    let code = get_code(&mut r, FIRST_DC_CB)?;
    let mut prev = ((code >> 1) as i32) ^ -((code & 1) as i32);
    c[0] = i16::try_from(prev).map_err(|_| bad("DC out of range"))?;
    let (mut code, mut sign) = (5u32, 0i32);
    for b in 1..blocks {
        code = get_code(&mut r, DC_CB[code.min(6) as usize])?;
        sign = if code == 0 { 0 } else { sign ^ -((code & 1) as i32) };
        prev += ((((code + 1) >> 1) as i32) ^ sign) - sign;
        c[b * 64] = i16::try_from(prev).map_err(|_| bad("DC out of range"))?;
    }
    if r.pos > end {
        return Err(bad("component overrun"));
    }
    let log2b = blocks.trailing_zeros();
    let mask = blocks - 1;
    let max = 64 << log2b;
    let (mut run, mut level) = (4u32, 2u32);
    let mut pos = mask;
    loop {
        let rem = end - r.pos;
        if rem == 0 || (rem < 57 && r.window() >> (64 - rem) == 0) {
            break;
        }
        run = get_code(&mut r, RUN_CB[run.min(15) as usize])?;
        pos += run as usize + 1;
        if pos >= max {
            return Err(bad("AC past the block"));
        }
        level = get_code(&mut r, LEV_CB[level.min(9) as usize])? + 1;
        let neg = r.get(1) != 0;
        if r.pos > end {
            return Err(bad("component overrun"));
        }
        let v = i16::try_from(level).map_err(|_| bad("AC out of range"))?;
        c[((pos & mask) << 6) + (pos >> log2b)] = if neg { -v } else { v };
    }
    let used = r.pos.div_ceil(8);
    Ok(Tail { zero_bytes: (end / 8 - used) as u32, empty: false })
}

/// The positions of block `b`'s coefficients that are nonzero, as bits (bit 0, the DC, is not
/// looked at).
#[inline(always)]
fn nonzeros(b: &[i16]) -> u64 {
    let mut m = 0u64;
    for (i, &v) in b.iter().enumerate().skip(1) {
        m |= ((v != 0) as u64) << i;
    }
    m
}

/// The codeword of `v` as [`codeword`] has it, with selects in place of the branch between the
/// Rice code and the exp-Golomb one (which values near the switch make hard to predict).
#[inline(always)]
fn codeword_u(cb: u8, v: u32) -> (u64, u32) {
    let s = (cb & 3) as u32;
    let rk = (cb >> 5) as u32;
    let ek = ((cb >> 2) & 7) as u32;
    let rice = ((1u64 << rk) | (v & ((1 << rk) - 1)) as u64, (v >> rk) + 1 + rk);
    let y = (v.wrapping_sub((s + 1) << rk)) as u64 + (1 << ek);
    let n = 64 - y.leading_zeros();
    let golomb = (y, (2 * n + s).wrapping_sub(ek));
    select_unpredictable(v < (s + 1) << rk, rice, golomb)
}

/// Appends `c` coded as [`decode`] reads it, then `tail`'s padding, to `out`.
pub fn encode(out: &mut Vec<u8>, c: &[i16], tail: Tail) {
    if tail.empty {
        debug_assert!(c.iter().all(|&v| v == 0));
        return;
    }
    let mut w = Out { out, acc: 0, n: 0 };
    let blocks = c.len() / 64;
    assert!(blocks <= 64);
    let mut prev = c[0] as i32;
    let (v, n) = codeword(FIRST_DC_CB, ((prev << 1) ^ (prev >> 31)) as u32);
    w.put(v, n);
    let (mut code, mut sign) = (5u32, 0i32);
    // The blocks with a nonzero coefficient at each scan position.
    let mut rows = [0u64; 64];
    for (b, k) in c.as_chunks::<64>().0.iter().enumerate() {
        if b > 0 {
            let v = k[0] as i32;
            let d = v - prev;
            prev = v;
            let cb = DC_CB[code.min(6) as usize];
            code = if d == 0 {
                0
            } else {
                let m = d.unsigned_abs();
                if (d < 0) != (sign != 0) { 2 * m - 1 } else { 2 * m }
            };
            sign = if code == 0 { 0 } else { sign ^ -((code & 1) as i32) };
            let (v, n) = codeword_u(cb, code);
            w.put(v, n);
        }
        let mut m = nonzeros(k);
        while m != 0 {
            rows[m.trailing_zeros() as usize] |= 1 << b;
            m &= m - 1;
        }
    }
    let (mut run, mut level) = (4u32, 2u32);
    let mut last = blocks - 1;
    for (i, &row) in rows.iter().enumerate().skip(1) {
        let mut m = row;
        while m != 0 {
            let b = m.trailing_zeros() as usize;
            m &= m - 1;
            let v = c[b * 64 + i];
            let pos = i * blocks + b;
            let (rv, rn) = codeword_u(RUN_CB[run.min(15) as usize], (pos - last - 1) as u32);
            run = (pos - last - 1) as u32;
            last = pos;
            let (lv, ln) = codeword_u(LEV_CB[level.min(9) as usize], v.unsigned_abs() as u32 - 1);
            level = v.unsigned_abs() as u32;
            let (lv, ln) = ((lv << 1) | (v < 0) as u64, ln + 1);
            if rn + ln <= 32 {
                w.put((rv << ln) | lv, rn + ln);
            } else {
                w.put(rv, rn);
                w.put(lv, ln);
            }
        }
    }
    w.finish();
    out.resize(out.len() + tail.zero_bytes as usize, 0);
}

#[cfg(test)]
mod tests {
    use super::*;
    use aqr_common::msb::padded;

    #[test]
    fn codewords_round_trip() {
        for cb in DC_CB.iter().chain(&RUN_CB).chain(&LEV_CB).chain(&[FIRST_DC_CB]) {
            let mut bytes = Vec::new();
            let mut w = Out { out: &mut bytes, acc: 0, n: 0 };
            for v in 0..3000 {
                let (x, n) = codeword(*cb, v);
                w.put(x, n);
            }
            w.finish();
            let d = padded(&bytes);
            let mut r = BitReader::new(&d, 0);
            for v in 0..3000 {
                assert_eq!(get_code(&mut r, *cb).unwrap(), v, "cb {cb:#x}");
            }
        }
    }

    #[test]
    fn coefficients_round_trip() {
        let mut rng = 12345u64;
        let mut next = || {
            rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (rng >> 33) as u32
        };
        for blocks in [1usize, 2, 4, 8, 16, 32] {
            let mut c = vec![0i16; blocks * 64];
            for (k, v) in c.iter_mut().enumerate() {
                let r = next();
                *v = if k % 64 == 0 {
                    (r % 2000) as i16 - 1000
                } else if r % 5 == 0 {
                    (next() % 41) as i16 - 20
                } else {
                    0
                };
            }
            let tail = Tail { zero_bytes: (blocks % 3) as u32, empty: false };
            let mut bytes = Vec::new();
            encode(&mut bytes, &c, tail);
            let d = padded(&bytes);
            let mut back = vec![0i16; blocks * 64];
            assert_eq!(decode(&d, 0, bytes.len() * 8, blocks, &mut back).unwrap(), tail);
            assert_eq!(back, c);
        }
    }

    /// All zeros, the largest values, one coefficient anywhere, and sparse rows far apart.
    #[test]
    fn extreme_coefficients_round_trip() {
        for blocks in [1usize, 2, 8, 32] {
            let mut sets: Vec<Vec<i16>> = vec![vec![0; blocks * 64]];
            sets.push((0..blocks * 64).map(|j| if (j / 64 + j % 64) % 2 == 0 { 32767 } else { -32767 }).collect());
            sets.push((0..blocks * 64).map(|j| if j % 64 == 0 { [32767, -32768][j / 64 % 2] } else { 0 }).collect());
            for at in [1, 2, 63, 64 * blocks - 1, 64 * blocks / 2 + 5] {
                let mut c = vec![0; blocks * 64];
                c[at] = -7;
                sets.push(c);
            }
            for c in sets {
                for zero_bytes in [0, 3] {
                    let tail = Tail { zero_bytes, empty: false };
                    let mut bytes = Vec::new();
                    encode(&mut bytes, &c, tail);
                    let d = padded(&bytes);
                    let mut back = vec![0i16; blocks * 64];
                    assert_eq!(decode(&d, 0, bytes.len() * 8, blocks, &mut back).unwrap(), tail);
                    assert_eq!(back, c);
                }
            }
        }
    }
}
