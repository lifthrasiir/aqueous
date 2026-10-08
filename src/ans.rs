//! Static rANS coding of a stream of LEB128 values (residuals, escapes), per block.
//!
//! Per value one symbol, its bit length and the two bits below the leading one (260
//! symbols), under a frequency table sent with the stream; the bits below those go raw to
//! a little-endian bit buffer. Two interleaved 32-bit rANS states with byte output.
//!
//! ```text
//! varint(symbols in the table) (varint(symbol - previous - 1) varint(frequency - 1))*
//! varint(rANS length) rANS bytes, raw bits
//! ```

use crate::varint::{Rd, put_uv};

const PROB_BITS: u32 = 12;
const M: u32 = 1 << PROB_BITS;
const L: u32 = 1 << 23;
/// Bits below the leading one in the symbol.
const TOPB: u32 = 2;
const NSYM: usize = 65 << TOPB;

/// The symbol of `u` and its raw bits (count, value).
#[inline]
fn split(u: u64) -> (usize, u32, u64) {
    let b = 64 - u.leading_zeros();
    if b < 2 {
        return ((b as usize) << TOPB, 0, 0);
    }
    let nb = b - 1;
    let top = nb.min(TOPB);
    let nraw = nb - top;
    let t = ((u >> nraw) as usize) & ((1 << top) - 1);
    (((b as usize) << TOPB) | t, nraw, u & ((1u64 << nraw) - 1))
}

/// The value of symbol `s` with raw bits `x`.
#[inline]
fn join(s: usize, x: u64) -> u64 {
    let b = (s >> TOPB) as u32;
    if b < 2 {
        return b as u64;
    }
    let nb = b - 1;
    let top = nb.min(TOPB);
    let t = (s as u64) & ((1 << top) - 1);
    (1u64 << nb) | t << (nb - top) | x
}

#[inline]
fn raw_bits(s: usize) -> u32 {
    let b = (s >> TOPB) as u32;
    if b < 2 { 0 } else { (b - 1).saturating_sub(TOPB) }
}

/// Scales `hist` to frequencies summing to M, every used symbol at least 1.
fn normalize(hist: &[u32; NSYM]) -> [u16; NSYM] {
    let mut f = [0u16; NSYM];
    let tot: u64 = hist.iter().map(|&x| x as u64).sum();
    let mut sum = 0i64;
    let mut best = 0;
    for s in 0..NSYM {
        if hist[s] > 0 {
            f[s] = ((hist[s] as u64 * M as u64 / tot) as u16).max(1);
            sum += f[s] as i64;
            if hist[s] > hist[best] {
                best = s;
            }
        }
    }
    let mut d = M as i64 - sum;
    if f[best] as i64 + d >= 1 {
        f[best] = (f[best] as i64 + d) as u16;
    } else {
        // Too many rare symbols: take from the largest.
        while d < 0 {
            let s = (0..NSYM).max_by_key(|&s| f[s]).unwrap();
            let take = (f[s] as i64 - 1).min(-d);
            f[s] -= take as u16;
            d += take;
        }
    }
    f
}

/// Codes the LEB128 values of `src` into `out` (appended). None if `src` holds a
/// non-canonical varint (it would not come back the same).
pub fn encode(src: &[u8], out: &mut Vec<u8>) -> Option<()> {
    let mut syms: Vec<u16> = Vec::with_capacity(src.len());
    let mut hist = [0u32; NSYM];
    let mut raw = Vec::with_capacity(src.len() / 2);
    let (mut acc, mut nacc) = (0u64, 0u32);
    let mut r = Rd::new(src);
    while !r.done() {
        let p = r.p;
        let u = r.uv();
        if r.overrun() || r.p - p != ((64 - (u | 1).leading_zeros()) as usize).div_ceil(7) {
            return None;
        }
        let (s, nraw, x) = split(u);
        syms.push(s as u16);
        hist[s] += 1;
        if nraw > 0 {
            acc |= x << nacc;
            if nacc + nraw >= 64 {
                raw.extend_from_slice(&acc.to_le_bytes());
                acc = x >> (64 - nacc);
                nacc = nacc + nraw - 64;
            } else {
                nacc += nraw;
            }
        }
    }
    raw.extend_from_slice(&acc.to_le_bytes()[..nacc.div_ceil(8) as usize]);
    let f = normalize(&hist);
    let mut cum = [0u32; NSYM];
    let mut c = 0;
    let mut used = 0;
    for s in 0..NSYM {
        cum[s] = c;
        c += f[s] as u32;
        used += (f[s] > 0) as u64;
    }
    put_uv(out, used);
    let mut last = -1i64;
    for s in 0..NSYM {
        if f[s] > 0 {
            put_uv(out, (s as i64 - last - 1) as u64);
            put_uv(out, f[s] as u64 - 1);
            last = s as i64;
        }
    }
    // rANS, back to front; symbol i on state i % 2.
    let mut rev: Vec<u8> = Vec::with_capacity(syms.len());
    let mut x = [L, L];
    for (i, &s) in syms.iter().enumerate().rev() {
        let (fr, cu) = (f[s as usize] as u32, cum[s as usize]);
        let xs = &mut x[i & 1];
        let max = ((L >> PROB_BITS) << 8) * fr;
        while *xs >= max {
            rev.push(*xs as u8);
            *xs >>= 8;
        }
        *xs = (*xs / fr << PROB_BITS) + *xs % fr + cu;
    }
    // Read back most significant byte first: state 0, then 1.
    for xs in [x[1], x[0]] {
        rev.extend_from_slice(&xs.to_le_bytes());
    }
    put_uv(out, rev.len() as u64);
    out.extend(rev.iter().rev());
    out.extend_from_slice(&raw);
    Some(())
}

/// Decodes into `out` (cleared) LEB128 values until it holds `n` bytes. False if the data
/// does not decode to exactly that (corrupt data may decode to something wrong: the file
/// checksum catches it).
pub fn decode(src: &[u8], n: usize, out: &mut Vec<u8>) -> bool {
    out.clear();
    let mut r = Rd::new(src);
    let used = r.uv() as usize;
    if used == 0 || used > NSYM {
        return false;
    }
    let mut f = [0u16; NSYM];
    let mut s = 0usize;
    let mut tot = 0u32;
    for i in 0..used {
        s += r.uv() as usize + (i > 0) as usize;
        let fr = r.uv() + 1;
        if s >= NSYM || fr > M as u64 {
            return false;
        }
        f[s] = fr as u16;
        tot += fr as u32;
    }
    if tot != M {
        return false;
    }
    // slot -> (symbol, frequency, cumulative)
    let mut slots = vec![(0u16, 0u16, 0u16); M as usize];
    let mut c = 0usize;
    for s in 0..NSYM {
        for e in &mut slots[c..c + f[s] as usize] {
            *e = (s as u16, f[s], c as u16);
        }
        c += f[s] as usize;
    }
    let len = r.uv() as usize;
    let Some(rans) = r.bytes(len) else { return false };
    let raw = &src[r.p..];
    let mut rp = Rd::new(rans);
    let mut x = [0u32; 2];
    for xs in x.iter_mut() {
        for _ in 0..4 {
            *xs = *xs << 8 | rp.u8() as u32;
        }
    }
    let mut bp = 0;
    let (mut acc, mut nacc) = (0u64, 0u32);
    let mut i = 0;
    while out.len() < n {
        let xs = &mut x[i & 1];
        i += 1;
        let slot = *xs & (M - 1);
        let (s, fr, cu) = slots[slot as usize];
        *xs = fr as u32 * (*xs >> PROB_BITS) + slot - cu as u32;
        // At least 2^11 here for valid data: two bytes reach L (a bound, for corrupt data).
        for _ in 0..2 {
            if *xs < L {
                *xs = *xs << 8 | rp.u8() as u32;
            }
        }
        let nraw = raw_bits(s as usize);
        let mut v = 0;
        if nraw > 0 {
            if nacc < nraw {
                let mut w = [0u8; 8];
                if let Some(t) = raw.get(bp..) {
                    let k = t.len().min(8);
                    w[..k].copy_from_slice(&t[..k]);
                }
                bp += 8;
                let fresh = u64::from_le_bytes(w);
                v = (acc | fresh << nacc) & ((1u64 << nraw) - 1);
                acc = fresh >> (nraw - nacc);
                nacc = 64 - (nraw - nacc);
            } else {
                v = acc & ((1u64 << nraw) - 1);
                acc >>= nraw;
                nacc -= nraw;
            }
        }
        put_uv(out, join(s as usize, v));
    }
    out.len() == n
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let mut x = 0x1234_5678_9ABC_DEF1u64;
        for case in 0..40 {
            let mut src = vec![];
            for i in 0..(5000 >> (case % 3 * 4)) + 1 {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                let v = match case % 4 {
                    0 => x >> (x % 64),
                    1 => x % 7,
                    2 => [0, 1, u64::MAX, 1 << 63, (1 << 63) - 1, 5, 6, 7][i % 8],
                    _ => (x >> 40) * (i as u64 % 3),
                };
                put_uv(&mut src, v);
            }
            let mut c = vec![];
            encode(&src, &mut c).unwrap();
            let mut d = vec![];
            assert!(decode(&c, src.len(), &mut d), "case {case}");
            assert_eq!(d, src, "case {case}");
        }
        assert!(encode(&[0x80, 0x00], &mut vec![]).is_none());
    }
}
