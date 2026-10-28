//! The Rice coding of tiles, `RICE_1` (the FITS Standard 4.0, §10.4.1), as its streams are
//! laid out (FORMAT.md, "Rice tiles"): the first pixel as it is, then the differences of
//! adjacent pixels in blocks, each block a code then its differences.
//!
//! An encoder picks each block's code; this crate does not reproduce any encoder's choice but
//! keeps the codes ([`Tile::codes`]), so that the stream is rebuilt from the pixels and the
//! codes: everything else in the stream follows from them.

use crate::bits::{Reader, Writer};

/// The parameters of a tile's coding: bytes per pixel (1, 2 or 4) and pixels per block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Params {
    pub bytepix: u8,
    pub block: u32,
}

impl Params {
    pub fn valid(&self) -> bool {
        matches!(self.bytepix, 1 | 2 | 4) && (1..=1 << 16).contains(&self.block)
    }

    /// Bits of a pixel.
    pub fn bits(&self) -> u32 {
        self.bytepix as u32 * 8
    }

    fn mask(&self) -> u32 {
        u32::MAX >> (32 - self.bits())
    }

    /// Bits of a block's code: 3, 4 or 5.
    pub fn code_bits(&self) -> u32 {
        match self.bytepix {
            1 => 3,
            2 => 4,
            _ => 5,
        }
    }

    /// The code of a block of differences stored as they are: 7, 15 or 26. Codes from 1 to
    /// it, excluded, are of Rice parameter `code - 1`; 0 is a block of zero differences.
    pub fn raw_code(&self) -> u8 {
        match self.bytepix {
            1 => 7,
            2 => 15,
            _ => 26,
        }
    }

    /// Blocks of a tile of `n` pixels.
    pub fn blocks(&self, n: usize) -> usize {
        n.div_ceil(self.block as usize)
    }
}

/// A tile taken apart: its pixels (of [`Params::bits`] bits each, the bits above zero) and its
/// blocks' codes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Tile {
    pub pixels: Vec<u32>,
    pub codes: Vec<u8>,
}

/// The differences of `pixels` (the first one against itself, so zero) as unsigned numbers,
/// 0, −1, 1, −2, ... to 0, 1, 2, 3, ..., in `p`'s bits.
pub fn differences(pixels: &[u32], p: Params, out: &mut Vec<u32>) {
    let (bits, mask) = (p.bits(), p.mask());
    let mut last = pixels.first().copied().unwrap_or(0);
    out.clear();
    out.extend(pixels.iter().map(|&x| {
        let d = x.wrapping_sub(last) & mask;
        last = x;
        // sign-extended, then zigzag
        let s = ((d << (32 - bits)) as i32) >> (32 - bits);
        ((s << 1) ^ (s >> 31)) as u32 & mask
    }));
}

/// The code that codes the block of differences `z` the shortest (the first of them, all
/// zero aside): the context of the codes kept (FORMAT.md).
pub fn shortest(z: &[u32], p: Params) -> u8 {
    if z.iter().all(|&v| v == 0) {
        return 0;
    }
    let raw = p.raw_code();
    let mut best = (z.len() as u64 * p.bits() as u64, raw);
    for k in 0..raw as u32 - 1 {
        let len: u64 = z.iter().map(|&v| (v >> k) as u64 + 1 + k as u64).sum();
        if len < best.0 {
            best = (len, k as u8 + 1);
        }
    }
    best.1
}

/// The Rice stream of `pixels` with blocks of `codes` (see [`Tile`]). None where the codes
/// do not fit the pixels (as many codes as blocks, 0 only for a block of zero differences, no
/// code past [`Params::raw_code`]).
pub fn encode(pixels: &[u32], codes: &[u8], p: Params) -> Option<Vec<u8>> {
    if pixels.is_empty() || codes.len() != p.blocks(pixels.len()) {
        return None;
    }
    let mut z = Vec::new();
    differences(pixels, p, &mut z);
    let mut w = Writer::with_capacity(pixels.len() * p.bytepix as usize / 2 + 16);
    w.put(pixels[0], p.bits());
    for (block, &code) in z.chunks(p.block as usize).zip(codes) {
        w.put(code as u32, p.code_bits());
        match code {
            0 if block.iter().all(|&v| v == 0) => {}
            0 => return None,
            c if c == p.raw_code() => block.iter().for_each(|&v| w.put(v, p.bits())),
            c if c < p.raw_code() => {
                let k = c as u32 - 1;
                for &v in block {
                    w.zeros(v >> k);
                    w.put(1, 1);
                    w.put(v, k);
                }
            }
            _ => return None,
        }
    }
    Some(w.finish())
}

/// Tile `d` of `n` pixels taken apart, or None where it is not a stream of [`encode`]'s
/// layout (it may still differ from what `encode` makes of the result, in its padding or what
/// follows it).
pub fn decode(d: &[u8], n: usize, p: Params) -> Option<Tile> {
    if n == 0 || !p.valid() {
        return None;
    }
    let (bits, mask) = (p.bits(), p.mask());
    let mut r = Reader::new(d);
    let mut last = r.get(bits);
    let mut t = Tile { pixels: Vec::with_capacity(n), codes: Vec::with_capacity(p.blocks(n)) };
    let mut first = true;
    let unzig = |z: u32| -> u32 { (z >> 1) ^ (z & 1).wrapping_neg() };
    for start in (0..n).step_by(p.block as usize) {
        let len = (n - start).min(p.block as usize);
        let code = r.get(p.code_bits()) as u8;
        t.codes.push(code);
        for _ in 0..len {
            let z = match code {
                0 => 0,
                c if c == p.raw_code() => r.get(bits),
                c if c < p.raw_code() => {
                    let k = c as u32 - 1;
                    let mut q = 0u32;
                    while r.bit() == 0 {
                        q += 1;
                        if q > mask >> k || r.past_end() {
                            return None;
                        }
                    }
                    q << k | r.get(k)
                }
                _ => return None,
            };
            if first {
                // the first difference is of the first pixel against itself
                if z != 0 {
                    return None;
                }
                first = false;
            }
            last = last.wrapping_add(unzig(z)) & mask;
            t.pixels.push(last);
        }
        if r.past_end() {
            return None;
        }
    }
    Some(t)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aqr_common::rng::Rng;

    #[test]
    fn round_trip() {
        let mut rng = Rng::new(1);
        for bytepix in [1u8, 2, 4] {
            let p = Params { bytepix, block: 32 };
            for n in [1usize, 31, 32, 33, 1000] {
                for spread in [0u32, 1, 100, 1 << 20, u32::MAX] {
                    let mut x = rng.u64() as u32;
                    let pixels: Vec<u32> = (0..n)
                        .map(|_| {
                            x = x.wrapping_add((rng.u64() as u32) % spread.max(1)).wrapping_sub(spread / 2);
                            x & p.mask()
                        })
                        .collect();
                    let mut z = Vec::new();
                    differences(&pixels, p, &mut z);
                    let codes: Vec<u8> = z.chunks(32).map(|b| shortest(b, p)).collect();
                    let d = encode(&pixels, &codes, p).unwrap();
                    let t = decode(&d, n, p).unwrap();
                    assert_eq!(t.pixels, pixels);
                    assert_eq!(t.codes, codes);
                    // every other code that is allowed codes it too
                    let raw: Vec<u8> = vec![p.raw_code(); codes.len()];
                    let d = encode(&pixels, &raw, p).unwrap();
                    assert_eq!(decode(&d, n, p).unwrap().pixels, pixels);
                }
            }
        }
    }
}
