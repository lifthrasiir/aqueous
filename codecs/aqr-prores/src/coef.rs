//! The entropy coding of a slice's component: its blocks' DC coefficients, differences coded
//! one after another, then the AC coefficients as runs and levels, interleaved across the
//! blocks (scan position by scan position). Codewords are Rice codes that switch to
//! exp-Golomb codes, their parameters picked by the previous value. Given the coefficients
//! the bits are unique, but for the zero padding at the end.
//!
//! Coefficients are kept by block, in scan order: `c[b * 64 + i]` is block `b`'s `i`th in
//! scan order (`c[b * 64]` its DC, as coded: without the 4096 offset).

use aqr_common::msb::{BitReader, BitWriter};

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

#[inline(always)]
fn put_code(w: &mut BitWriter, cb: u8, v: u32) {
    let s = (cb & 3) as u32;
    let rk = (cb >> 5) as u32;
    let ek = ((cb >> 2) & 7) as u32;
    if v < (s + 1) << rk {
        w.put(1, (v >> rk) + 1);
        w.put(v & ((1 << rk) - 1), rk);
    } else {
        let y = (v - ((s + 1) << rk)) as u64 + (1 << ek);
        let n = 64 - y.leading_zeros();
        let zeros = n - 1 - ek + s + 1;
        w.put(0, zeros.min(32));
        w.put(0, zeros.saturating_sub(32));
        if n > 32 {
            w.put((y >> 32) as u32, n - 32);
            w.put(y as u32, 32);
        } else {
            w.put(y as u32, n);
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

/// Encodes `c` as [`decode`] reads it, then `tail`'s padding.
pub fn encode(w: &mut BitWriter, c: &[i16], tail: Tail) {
    if tail.empty {
        debug_assert!(c.iter().all(|&v| v == 0));
        return;
    }
    let blocks = c.len() / 64;
    let mut prev = c[0] as i32;
    put_code(w, FIRST_DC_CB, ((prev << 1) ^ (prev >> 31)) as u32);
    let (mut code, mut sign) = (5u32, 0i32);
    for b in 1..blocks {
        let v = c[b * 64] as i32;
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
        put_code(w, cb, code);
    }
    let (mut run, mut level) = (4u32, 2u32);
    let mut last = blocks - 1;
    for i in 1..64 {
        for b in 0..blocks {
            let v = c[b * 64 + i];
            if v == 0 {
                continue;
            }
            let pos = i * blocks + b;
            let cb = RUN_CB[run.min(15) as usize];
            run = (pos - last - 1) as u32;
            put_code(w, cb, run);
            last = pos;
            let cb = LEV_CB[level.min(9) as usize];
            level = v.unsigned_abs() as u32;
            put_code(w, cb, level - 1);
            w.put((v < 0) as u32, 1);
        }
    }
    let pad = (8 - w.len() % 8) % 8;
    w.put(0, pad as u32);
    for _ in 0..tail.zero_bytes {
        w.put(0, 8);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aqr_common::msb::padded;

    #[test]
    fn codewords_round_trip() {
        for cb in DC_CB.iter().chain(&RUN_CB).chain(&LEV_CB).chain(&[FIRST_DC_CB]) {
            let mut w = BitWriter::new();
            for v in 0..3000 {
                put_code(&mut w, *cb, v);
            }
            let d = padded(&w.finish());
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
            let mut w = BitWriter::new();
            encode(&mut w, &c, tail);
            let bytes = w.finish();
            let d = padded(&bytes);
            let mut back = vec![0i16; blocks * 64];
            assert_eq!(decode(&d, 0, bytes.len() * 8, blocks, &mut back).unwrap(), tail);
            assert_eq!(back, c);
        }
    }
}
