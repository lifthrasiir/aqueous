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

/// Bytes of the model.
pub const MODEL_BYTES: u64 = (CONTEXTS * 8) as u64;

fn model() -> Vec<[u16; 4]> {
    vec![[16384; 4]; CONTEXTS]
}

/// Bases packed four to a byte, the first in the low bits: what the encoder holds of a file's.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Packed {
    bytes: Vec<u8>,
    len: usize,
}

impl Packed {
    /// Room for `n` bases.
    pub fn with_capacity(n: usize) -> Packed {
        Packed { bytes: Vec::with_capacity(n.div_ceil(4)), len: 0 }
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    #[inline(always)]
    pub fn push(&mut self, b: u8) {
        let shift = 2 * (self.len & 3);
        if shift == 0 {
            self.bytes.push(b & 3);
        } else {
            *self.bytes.last_mut().unwrap() |= (b & 3) << shift;
        }
        self.len += 1;
    }

    /// Bases `r`, 0 to 3 each.
    pub fn iter(&self, r: std::ops::Range<usize>) -> impl Iterator<Item = u8> + '_ {
        r.map(|i| self.bytes[i / 4] >> (2 * (i & 3)) & 3)
    }
}

/// Packs bases as they come, four at a time.
pub struct Packer {
    p: Packed,
    /// The bases not yet in, from the low bits, and how many.
    word: u8,
    n: u32,
}

impl Packed {
    /// `f` of each of bases `r` in order, a byte of them at a time.
    #[inline(always)]
    fn each(&self, r: std::ops::Range<usize>, mut f: impl FnMut(usize)) {
        let (mut i, end) = (r.start, r.end);
        while i < end && i % 4 != 0 {
            f((self.bytes[i / 4] >> (2 * (i & 3)) & 3) as usize);
            i += 1;
        }
        while i + 4 <= end {
            let b = self.bytes[i / 4];
            f((b & 3) as usize);
            f((b >> 2 & 3) as usize);
            f((b >> 4 & 3) as usize);
            f((b >> 6) as usize);
            i += 4;
        }
        while i < end {
            f((self.bytes[i / 4] >> (2 * (i & 3)) & 3) as usize);
            i += 1;
        }
    }
}

impl Packer {
    /// Room for `n` bases.
    pub fn with_capacity(n: usize) -> Packer {
        Packer { p: Packed::with_capacity(n), word: 0, n: 0 }
    }

    #[inline(always)]
    pub fn push(&mut self, b: u8) {
        self.word |= (b & 3) << (2 * self.n);
        self.n += 1;
        if self.n == 4 {
            self.p.bytes.push(self.word);
            self.p.len += 4;
            (self.word, self.n) = (0, 0);
        }
    }

    pub fn finish(mut self) -> Packed {
        if self.n > 0 {
            self.p.bytes.push(self.word);
            self.p.len += self.n as usize;
        }
        self.p
    }
}

impl FromIterator<u8> for Packed {
    fn from_iter<I: IntoIterator<Item = u8>>(bases: I) -> Packed {
        let mut p = Packed::default();
        bases.into_iter().for_each(|b| p.push(b));
        p
    }
}

/// Bases coded at a time by [`encode`]: the model is run over all of them first, kept at the
/// start of each such chunk, then over each chunk again, from the last, as the coder goes
/// backward (the next chunk's on a worker of the pool while this one's is coded, where one is
/// free). A chunk's symbols take 4 bytes a base, a model kept [`MODEL_BYTES`]: of chr1 of hg19
/// (225 M bases), 2 × 34 MB and 55 MB, where the symbols of all the bases took 0.9 GB.
const CHUNK: usize = 8 << 20;

/// The coded form of bases `s`.
pub fn encode(s: &Packed) -> Vec<u8> {
    encode_in(s, CHUNK)
}

/// The start and frequency of each base of `s` in `r` ([`encode`]), the model `t` going on from
/// context `h`, into `syms`.
fn symbols(s: &Packed, r: std::ops::Range<usize>, t: &mut [[u16; 4]], mut h: usize, syms: &mut Vec<(u16, u16)>) {
    syms.clear();
    s.each(r, |b| {
        let p = &mut t[h & (CONTEXTS - 1)];
        let f = freqs(p);
        let start: u32 = f[..b].iter().sum();
        syms.push((start as u16, f[b] as u16));
        update(p, b);
        h = h << 2 | b;
    });
}

/// [`encode`], the bases coded `chunk` at a time.
fn encode_in(s: &Packed, chunk: usize) -> Vec<u8> {
    let n = s.len();
    let last = n.saturating_sub(1) / chunk * chunk;
    // the model at the start of each chunk, and the context there; the last one's is where the
    // model is left
    let mut t = model();
    let mut kept: Vec<(Vec<[u16; 4]>, usize)> = Vec::with_capacity(last / chunk);
    let mut h = 0usize;
    for start in (0..last).step_by(chunk) {
        kept.push((t.clone(), h));
        s.each(start..start + chunk, |b| {
            update(&mut t[h & (CONTEXTS - 1)], b);
            h = h << 2 | b;
        });
    }
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
    let mut out = Vec::with_capacity(n / 4 + 8);
    // the chunks from the last, the coder backward
    let mut syms = Vec::with_capacity(chunk.min(n));
    symbols(s, last..n, &mut t, h, &mut syms);
    drop(t);
    let mut spare = Vec::new();
    let mut start = last;
    aqr_common::pool::scope(1, |sc| {
        loop {
            // the chunk before's symbols, on a worker if there is one, else once this one's are coded
            let before = kept.pop().map(|(mut t, h)| {
                start -= chunk;
                let (r, mut syms) = (start..start + chunk, std::mem::take(&mut spare));
                move || {
                    syms.reserve(chunk);
                    symbols(s, r, &mut t, h, &mut syms);
                    syms
                }
            });
            let (mut later, mut now) = (None, None);
            match before {
                Some(job) if sc.workers() > 0 => now = Some(sc.spawn(job)),
                job => later = job,
            }
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
            let next = match (now, later) {
                (Some(h), _) => h.join().unwrap_or_else(|p| std::panic::resume_unwind(p)),
                (None, Some(job)) => job(),
                (None, None) => break,
            };
            spare = std::mem::replace(&mut syms, next);
        }
    });
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
                let c = encode(&s.iter().copied().collect());
                assert_eq!(decode(&c, s.len()).unwrap(), s);
                // (coded a chunk at a time alike, of a whole number of chunks too)
                for chunk in [1, n / 7 + 1, n / 2, n].into_iter().filter(|&c| c > 0 && n / c < 64) {
                    assert!(encode_in(&s.iter().copied().collect(), chunk) == c, "{n} by {chunk}");
                }
                assert!(decode(&c, s.len() + 1).is_err());
                if !c.is_empty() {
                    assert!(decode(&c[..c.len() - 1], s.len()).is_err());
                }
            }
        }
        let r: Vec<u8> = (0..100_000).map(|i| ((i * 7 / 3) % 4) as u8).collect();
        assert!(encode(&r.into_iter().collect()).len() < 1000);
    }
}
