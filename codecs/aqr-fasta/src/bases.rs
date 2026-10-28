//! The bases (0..4 for A, C, G, T) coded by an adaptive order-9 model and a 32-bit rANS
//! (FORMAT.md, "Bases"): per context of the 9 bases before, four 16-bit probabilities summing
//! to 2^16 that move by 1/64 towards each base seen; the top 12 bits of the first three are
//! its frequencies, the fourth what is left of 4096.

use crate::{Result, corrupt};

/// Bases of the context: 9, a table of 2 MiB, so that several decoders at once keep theirs
/// close (order 10 codes 1% smaller, but takes 8 MiB and decodes 3 times slower then).
pub const ORDER: u32 = 9;
const CONTEXTS: usize = 1 << (2 * ORDER);
/// The adaptation: probabilities move by `1/2^RATE` of the way, but none below `FLOOR`.
const RATE: u32 = 6;
const FLOOR: u16 = 64;
/// The bits of the frequencies, and the lower bound of the coder's state.
const SCALE: u32 = 12;
const LOW: u32 = 1 << 23;

#[inline(always)]
fn freqs(p: &[u16; 4]) -> [u32; 4] {
    let f0 = (p[0] >> 4) as u32;
    let f1 = (p[1] >> 4) as u32;
    let f2 = (p[2] >> 4) as u32;
    [f0, f1, f2, (1 << SCALE) - f0 - f1 - f2]
}

#[inline(always)]
fn update(p: &mut [u16; 4], b: usize) {
    let mut sum = 0u32;
    for q in p.iter_mut() {
        *q -= (*q - FLOOR) >> RATE;
        sum += *q as u32;
    }
    p[b] += (65536 - sum) as u16;
}

/// Has the cache line at `p` loaded, where the target has a way to (the output does not depend
/// on it).
#[inline(always)]
fn prefetch(p: *const [u16; 4]) {
    #[cfg(target_arch = "aarch64")]
    // SAFETY: a hint, which reads nothing
    unsafe {
        std::arch::asm!("prfm pldl1keep, [{0}]", in(reg) p, options(nostack, readonly, preserves_flags));
    }
    #[cfg(target_arch = "x86_64")]
    // SAFETY: as above
    unsafe {
        std::arch::x86_64::_mm_prefetch::<{ std::arch::x86_64::_MM_HINT_T0 }>(p as *const i8);
    }
    let _ = p;
}

fn model() -> Vec<[u16; 4]> {
    vec![[16384; 4]; CONTEXTS]
}

/// The coded form of bases `s`.
pub fn encode(s: &[u8]) -> Vec<u8> {
    let mut t = model();
    // the model forward, the coder backward: the start and frequency of each base
    let mut syms: Vec<(u16, u16)> = Vec::with_capacity(s.len());
    let mut h = 0usize;
    for &b in s {
        let b = (b & 3) as usize;
        let p = &mut t[h & (CONTEXTS - 1)];
        let f = freqs(p);
        let start: u32 = f[..b].iter().sum();
        syms.push((start as u16, f[b] as u16));
        update(p, b);
        h = h << 2 | b;
    }
    drop(t);
    // (x / freq by a multiplication: the same quotient, as in ryg_rans)
    let rcp: Vec<(u32, u32)> = (0..=1u64 << SCALE)
        .map(|f| match f {
            0 | 1 => (u32::MAX, 0),
            f => {
                let shift = 64 - (f - 1).leading_zeros() as u64;
                ((1u64 << (shift + 31)).div_ceil(f) as u32, shift as u32 - 1)
            }
        })
        .collect();
    let mut x = LOW;
    let mut out = Vec::with_capacity(s.len() / 4 + 8);
    for &(start, freq) in syms.iter().rev() {
        let (start, freq) = (start as u32, freq as u32);
        let max = ((LOW >> SCALE) << 8) * freq;
        while x >= max {
            out.push(x as u8);
            x >>= 8;
        }
        // x + start + q * (2^SCALE - freq), q = x / freq: (q << SCALE) + x % freq + start
        x = if freq == 1 {
            (x << SCALE) + start
        } else {
            let (r, shift) = rcp[freq as usize];
            let q = (((x as u64 * r as u64) >> 32) as u32) >> shift;
            x + start + q * ((1 << SCALE) - freq)
        };
    }
    out.extend_from_slice(&x.to_le_bytes());
    out.reverse();
    out
}

/// The `n` bases of coded form `c`; fails where `c` is not exactly of them.
pub fn decode(c: &[u8], n: usize) -> Result<Vec<u8>> {
    let mut d = Decoder::new(c, n)?;
    let s = d.by_ref().collect();
    d.finish()?;
    Ok(s)
}

/// The `n` bases of coded form `c` as they are decoded, without the bases themselves held
/// ([`Decoder::finish`] tells whether `c` was exactly of them).
pub struct Decoder<'c> {
    t: Vec<[u16; 4]>,
    c: &'c [u8],
    at: usize,
    x: u32,
    h: usize,
    left: usize,
}

impl<'c> Decoder<'c> {
    pub fn new(c: &'c [u8], n: usize) -> Result<Self> {
        let x = u32::from_be_bytes(*c.first_chunk().ok_or_else(corrupt)?);
        Ok(Decoder { t: model(), c, at: 4, x, h: 0, left: n })
    }

    /// Whether every base was decoded, the state back where the coder started, every byte read.
    pub fn finish(self) -> Result<()> {
        if self.left != 0 || self.x != LOW || self.at != self.c.len() {
            return Err(corrupt());
        }
        Ok(())
    }

    #[inline(always)]
    fn byte(&mut self) -> u32 {
        let b = self.c.get(self.at).copied().unwrap_or(0);
        self.at += 1;
        b as u32
    }
}

impl Iterator for Decoder<'_> {
    type Item = u8;

    #[inline(always)]
    fn next(&mut self) -> Option<u8> {
        if self.left == 0 {
            return None;
        }
        self.left -= 1;
        let (t, h, mut x) = (&mut self.t, self.h, self.x);
        // the contexts two bases on, one cache line of them: loaded as this one decodes
        prefetch(t.as_ptr().wrapping_add((h << 4) & (CONTEXTS - 1)));
        let p = &mut t[h & (CONTEXTS - 1)];
        let f = freqs(p);
        let slot = x & ((1 << SCALE) - 1);
        let (c1, c2) = (f[0], f[0] + f[1]);
        let c3 = c2 + f[2];
        // (without branches: the base is not predictable)
        let b = (slot >= c1) as usize + (slot >= c2) as usize + (slot >= c3) as usize;
        let cum = [0, c1, c2, c3];
        x = f[b] * (x >> SCALE) + slot - cum[b];
        update(p, b);
        self.h = h << 2 | b;
        if x < LOW {
            x = x << 8 | self.byte();
            if x < LOW {
                x = x << 8 | self.byte();
            }
        }
        self.x = x;
        Some(b as u8)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.left, Some(self.left))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        for n in [0, 1, 2, 5, 1000, 100_000] {
            let s: Vec<u8> = aqr_common::rng::noise(n).iter().map(|&b| b & 3).collect();
            // (and a repetitive one, which codes small)
            let r: Vec<u8> = (0..n).map(|i| ((i * 7 / 3) % 4) as u8).collect();
            for s in [s, r] {
                let c = encode(&s);
                assert_eq!(decode(&c, s.len()).unwrap(), s);
                assert!(decode(&c, s.len() + 1).is_err());
                if !c.is_empty() {
                    assert!(decode(&c[..c.len() - 1], s.len()).is_err());
                }
            }
        }
        let r: Vec<u8> = (0..100_000).map(|i| ((i * 7 / 3) % 4) as u8).collect();
        assert!(encode(&r).len() < 1000);
    }
}
