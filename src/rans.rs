//! Static-model rANS (after png-recons' coder, with alphabets of up to [`MAX_ALPHA`]
//! symbols): per-table frequencies counted in a first pass and stored as quantised
//! logarithms, four interleaved 32-bit states with 16-bit renormalisation.

use crate::{Error, Result};

pub const PROB_BITS: u32 = 12;
pub const PROB_SCALE: u32 = 1 << PROB_BITS;
pub const MAX_ALPHA: usize = 512;
/// States stay below 2^31, where the reciprocal division of the encoder is exact.
const RANS_L: u32 = 1 << 15;

/// Frequencies are stored as quantised logarithms, `STEPS` per octave; the weight of level
/// `l` (1..) is about `2^(l / STEPS)`. Level 0 means the symbol is absent.
const STEPS: u32 = 4;
const LEVELS: usize = 64;

/// Integer weights of the levels: round(2^((l - 1) / STEPS) * 256), from a table of
/// 2^(k/4) in fixed point, so that both sides agree bit for bit.
static WEIGHT: [u64; LEVELS] = {
    const FRAC: [u64; 4] = [65536, 77936, 92682, 110218];
    let mut t = [0u64; LEVELS];
    let mut l = 1;
    while l < LEVELS {
        let k = (l - 1) as u32;
        t[l] = (FRAC[(k % STEPS) as usize] << (k / STEPS)) >> 8;
        l += 1;
    }
    t
};

/// The level of `count` in a table whose largest count is `max`.
fn level_of(count: u32, max: u32) -> u8 {
    if count == 0 {
        return 0;
    }
    // log2 by integer steps: the largest count maps to the top level.
    let l = (LEVELS - 1) as f64 + (STEPS as f64 * (count as f64 / max as f64).log2()).round();
    l.max(1.0) as u8
}

/// Normalised frequencies (summing to `PROB_SCALE`) from quantised levels (both sides).
fn freqs_from_levels(levels: &[u8]) -> Result<Vec<u32>> {
    let n = levels.len();
    let nz = levels.iter().filter(|&&l| l > 0).count();
    let mut f = vec![0u32; n];
    if nz == 0 {
        f[0] = PROB_SCALE;
        return Ok(f);
    }
    if nz > PROB_SCALE as usize {
        return Err(Error::Corrupt);
    }
    let sum: u64 = levels.iter().map(|&l| WEIGHT[l as usize]).sum();
    let mut tot = 0u32;
    for i in 0..n {
        if levels[i] > 0 {
            f[i] = ((WEIGHT[levels[i] as usize] * PROB_SCALE as u64 / sum) as u32).max(1);
            tot += f[i];
        }
    }
    // Fix the rounding on the largest frequencies (first index wins ties).
    while tot != PROB_SCALE {
        let m = (0..n).max_by_key(|&i| (f[i], std::cmp::Reverse(i))).unwrap();
        if tot < PROB_SCALE {
            f[m] += PROB_SCALE - tot;
            tot = PROB_SCALE;
        } else {
            let d = (tot - PROB_SCALE).min(f[m] - 1);
            if d == 0 {
                return Err(Error::Corrupt);
            }
            f[m] -= d;
            tot -= d;
        }
    }
    Ok(f)
}

/// Writes the levels of `counts` (a used table) and returns the frequencies both sides use.
pub fn write_table(w: &mut BitW, counts: &[u32]) -> Vec<u32> {
    let n = counts.len();
    let max = counts.iter().copied().max().unwrap_or(0);
    let levels: Vec<u8> = counts.iter().map(|&c| level_of(c, max)).collect();
    // Each level as the zigzag difference from the previous one (Elias gamma); absent
    // symbols as runs.
    let mut prev = 0i32;
    let mut i = 0;
    while i < n {
        let l = levels[i] as i32;
        if l == 0 {
            let mut run = 0;
            while i + run < n && levels[i + run] == 0 {
                run += 1;
            }
            w.gamma(0);
            w.gamma(run as u32 - 1);
            i += run;
            continue;
        }
        let d = l - prev;
        let z = if d > 0 { 2 * d as u32 - 1 } else { 2 * (-d) as u32 };
        w.gamma(z + 1);
        prev = l;
        i += 1;
    }
    freqs_from_levels(&levels).expect("valid levels")
}

pub fn read_table(r: &mut BitR, n: usize) -> Result<Vec<u32>> {
    let mut levels = vec![0u8; n];
    let mut prev = 0i32;
    let mut i = 0;
    while i < n {
        let z = r.gamma()?;
        if z == 0 {
            let run = r.gamma()? as usize + 1;
            if i + run > n {
                return Err(Error::Corrupt);
            }
            i += run;
            continue;
        }
        let z = z - 1;
        let d = if z & 1 == 1 { (z as i32 + 1) / 2 } else { -(z as i32 / 2) };
        let l = prev + d;
        if !(1..LEVELS as i32).contains(&l) {
            return Err(Error::Corrupt);
        }
        levels[i] = l as u8;
        prev = l;
        i += 1;
    }
    freqs_from_levels(&levels)
}

/// Estimated bits to code `counts` with its own exact table (data only).
pub fn cost(counts: &[u32]) -> f64 {
    let tot: f64 = counts.iter().map(|&c| c as f64).sum();
    counts.iter().filter(|&&c| c > 0).map(|&c| c as f64 * (tot / c as f64).log2()).sum()
}

/// Bits to code `counts` with the frequencies `f`.
pub fn cost_with(counts: &[u32], f: &[u32]) -> f64 {
    counts
        .iter()
        .zip(f)
        .filter(|&(&c, _)| c > 0)
        .map(|(&c, &q)| c as f64 * (PROB_BITS as f64 - (q.max(1) as f64).log2()))
        .sum()
}

/// Decoding tables, flat: per table the symbol of every slot, and per symbol its frequency
/// and cumulative frequency (`stride` symbols per table).
pub struct Tables {
    lut: Vec<u16>,
    fc: Vec<u32>,
    stride: usize,
}

impl Tables {
    pub fn new(freqs: &[Vec<u32>], stride: usize) -> Tables {
        let mut lut = vec![0u16; freqs.len() * PROB_SCALE as usize];
        let mut fc = vec![0u32; freqs.len() * stride];
        for (t, f) in freqs.iter().enumerate() {
            let l = &mut lut[t * PROB_SCALE as usize..];
            let mut acc = 0u32;
            for (s, &fs) in f.iter().enumerate() {
                fc[t * stride + s] = fs | acc << 16;
                l[acc as usize..(acc + fs) as usize].fill(s as u16);
                acc += fs;
            }
        }
        Tables { lut, fc, stride }
    }

    pub fn len(&self) -> usize {
        self.fc.len() / self.stride.max(1)
    }

    pub fn is_empty(&self) -> bool {
        self.fc.is_empty()
    }
}

/// A symbol's encoding parameters, packed: the reciprocal of its frequency (after
/// ryg_rans, division by a multiplication), the bias, the frequency and the shift.
#[inline(always)]
fn enc_sym(start: u32, freq: u32) -> u64 {
    let (rcp, shift, bias) = if freq < 2 {
        (!0u32, 0u32, start + PROB_SCALE - 1)
    } else {
        let mut shift = 0;
        while freq > 1 << shift {
            shift += 1;
        }
        ((1u64 << (shift + 31)).div_ceil(freq as u64) as u32, shift - 1, start)
    };
    rcp as u64 | (bias as u64) << 32 | (freq as u64) << 45 | (shift as u64) << 58
}

#[inline(always)]
fn put(x: &mut u32, e: u64, out: &mut [u8], pos: &mut usize) {
    let freq = ((e >> 45) & 0x1fff) as u32;
    let bias = ((e >> 32) & 0x1fff) as u32;
    let shift = (e >> 58) as u32;
    let rcp = e as u32;
    let renorm = *x >= freq << (31 - PROB_BITS);
    out[*pos - 2..*pos].copy_from_slice(&(*x as u16).to_le_bytes());
    *pos -= 2 * renorm as usize;
    let y = if renorm { *x >> 16 } else { *x };
    let q = ((y as u64 * rcp as u64) >> 32) as u32 >> shift;
    *x = y.wrapping_add(bias).wrapping_add(q.wrapping_mul(PROB_SCALE - freq));
}

/// Encodes `syms` (table, symbol) in order; returns the stream the decoder reads forward.
pub fn encode(syms: &[(u16, u16)], freqs: &[Vec<u32>], stride: usize) -> Vec<u8> {
    let mut enc = vec![0u64; freqs.len() * stride];
    for (t, f) in freqs.iter().enumerate() {
        let mut acc = 0;
        for (s, &fs) in f.iter().enumerate() {
            if fs > 0 {
                enc[t * stride + s] = enc_sym(acc, fs);
            }
            acc += fs;
        }
    }
    let mut out = vec![0u8; 2 * syms.len() + 32];
    let mut pos = out.len();
    let mut x = [RANS_L; 4];
    let e = |(t, s): (u16, u16)| enc[t as usize * stride + s as usize];
    let mut i = syms.len();
    while !i.is_multiple_of(4) {
        i -= 1;
        put(&mut x[i & 3], e(syms[i]), &mut out, &mut pos);
    }
    let [mut x0, mut x1, mut x2, mut x3] = x;
    while i > 0 {
        i -= 4;
        put(&mut x3, e(syms[i + 3]), &mut out, &mut pos);
        put(&mut x2, e(syms[i + 2]), &mut out, &mut pos);
        put(&mut x1, e(syms[i + 1]), &mut out, &mut pos);
        put(&mut x0, e(syms[i]), &mut out, &mut pos);
    }
    for x in [x3, x2, x1, x0] {
        pos -= 4;
        out[pos..pos + 2].copy_from_slice(&((x >> 16) as u16).to_le_bytes());
        out[pos + 2..pos + 4].copy_from_slice(&(x as u16).to_le_bytes());
    }
    out.drain(..pos);
    out
}

/// The decoder: reads the stream forward. Reading past the end yields zeros (checked at the
/// end with [`Decoder::finished`]).
#[derive(Clone, Copy)]
pub struct Decoder<'a> {
    st: [u32; 4],
    data: &'a [u8],
    pos: usize,
}

impl<'a> Decoder<'a> {
    pub fn new(data: &'a [u8]) -> Decoder<'a> {
        let mut d = Decoder { st: [0; 4], data, pos: 0 };
        for k in 0..4 {
            let hi = d.word();
            let lo = d.word();
            d.st[k] = hi << 16 | lo;
        }
        d
    }

    #[inline(always)]
    fn word(&mut self) -> u32 {
        let w = match self.data.get(self.pos..self.pos + 2) {
            Some(b) => u16::from_le_bytes([b[0], b[1]]),
            None => 0,
        };
        self.pos += 2;
        w as u32
    }

    #[inline(always)]
    fn advance(&mut self, x: u32) {
        let w = match self.data.get(self.pos..self.pos + 2) {
            Some(b) => u16::from_le_bytes([b[0], b[1]]) as u32,
            None => 0,
        };
        let need = x < RANS_L;
        let x = if need { x << 16 | w } else { x };
        self.pos += 2 * need as usize;
        self.st = [self.st[1], self.st[2], self.st[3], x];
    }

    /// Decodes a symbol of table `t` (`t < tb.len()`, checked by the caller).
    #[inline(always)]
    pub fn get(&mut self, tb: &Tables, t: usize) -> u32 {
        let x = self.st[0];
        let slot = x & (PROB_SCALE - 1);
        debug_assert!(t < tb.len());
        // SAFETY: `t` is below the number of tables (the caller's invariant), `slot` below
        // `PROB_SCALE`, and every symbol in the LUT below the stride.
        let (s, e) = unsafe {
            let s = *tb.lut.get_unchecked(t * PROB_SCALE as usize + slot as usize) as usize;
            (s, *tb.fc.get_unchecked(t * tb.stride + s))
        };
        self.advance((e & 0xffff) * (x >> PROB_BITS) + slot - (e >> 16));
        s as u32
    }

    pub fn finished(&self) -> bool {
        self.pos == self.data.len() && self.st.iter().all(|&x| x == RANS_L)
    }
}

/// LSB-first bit writer for side information and raw bits.
#[derive(Default)]
pub struct BitW {
    out: Vec<u8>,
    acc: u64,
    n: u32,
}

impl BitW {
    #[inline(always)]
    pub fn bits(&mut self, k: u32, v: u32) {
        debug_assert!(k <= 32);
        self.acc |= ((v as u64) & ((1u64 << k) - 1)) << self.n;
        self.n += k;
        if self.n >= 32 {
            self.out.extend_from_slice(&(self.acc as u32).to_le_bytes());
            self.acc >>= 32;
            self.n -= 32;
        }
    }

    pub fn gamma(&mut self, v: u32) {
        // Elias gamma of v + 1.
        let x = v as u64 + 1;
        let nb = 64 - x.leading_zeros();
        self.bits(nb - 1, 0);
        for i in (0..nb).rev() {
            self.bits(1, (x >> i) as u32 & 1);
        }
    }

    pub fn bit_len(&self) -> usize {
        self.out.len() * 8 + self.n as usize
    }

    pub fn finish(mut self) -> Vec<u8> {
        while self.n > 0 {
            self.out.push(self.acc as u8);
            self.acc >>= 8;
            self.n = self.n.saturating_sub(8);
        }
        self.out
    }
}

/// LSB-first bit reader; past the end it reads zeros and records the overrun.
pub struct BitR<'a> {
    data: &'a [u8],
    pos: usize,
    acc: u64,
    n: u32,
    over: usize,
}

impl<'a> BitR<'a> {
    pub fn new(data: &'a [u8]) -> BitR<'a> {
        let mut r = BitR { data, pos: 0, acc: 0, n: 0, over: 0 };
        r.refill();
        r
    }

    #[inline(always)]
    fn refill(&mut self) {
        if let Some(b) = self.data.get(self.pos..self.pos + 8) {
            let w = u64::from_le_bytes(b.try_into().unwrap());
            self.acc |= w << self.n;
            let take = (63 - self.n) >> 3;
            self.pos += take as usize;
            self.n += take * 8;
        } else {
            while self.n <= 56 {
                let b = match self.data.get(self.pos) {
                    Some(&b) => b,
                    None => {
                        self.over += 1;
                        0
                    }
                };
                self.acc |= (b as u64) << self.n;
                self.pos += 1;
                self.n += 8;
            }
        }
    }

    #[inline(always)]
    pub fn bits(&mut self, k: u32) -> u32 {
        debug_assert!(k <= 32);
        if self.n < k {
            self.refill();
        }
        let v = (self.acc & ((1u64 << k) - 1)) as u32;
        self.acc >>= k;
        self.n -= k;
        v
    }

    pub fn gamma(&mut self) -> Result<u32> {
        let mut z = 0;
        while self.bits(1) == 0 {
            z += 1;
            if z > 32 || self.overrun() {
                return Err(Error::Corrupt);
            }
        }
        let mut x = 1u64;
        for _ in 0..z {
            x = x << 1 | self.bits(1) as u64;
        }
        u32::try_from(x - 1).map_err(|_| Error::Corrupt)
    }

    /// Whether more bits were read than there are (past the final byte's padding).
    pub fn overrun(&self) -> bool {
        // Bytes fetched beyond the data that were also consumed.
        self.over * 8 > self.n as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let mut syms = vec![];
        let mut counts = vec![vec![0u32; 300]; 3];
        let mut x = 12345u32;
        for i in 0..100_000 {
            x = x.wrapping_mul(1103515245).wrapping_add(12345);
            let t = (i % 3) as u16;
            let s = ((x >> 16) % (50 + 100 * t as u32)) as u16;
            let s = if x & 7 == 0 { s } else { s / 8 };
            syms.push((t, s));
            counts[t as usize][s as usize] += 1;
        }
        let mut w = BitW::default();
        let freqs: Vec<Vec<u32>> = counts.iter().map(|c| write_table(&mut w, c)).collect();
        let tb = w.finish();
        let st = encode(&syms, &freqs, 300);
        let mut r = BitR::new(&tb);
        let back: Vec<Vec<u32>> = (0..3).map(|_| read_table(&mut r, 300).unwrap()).collect();
        assert_eq!(back, freqs);
        let tables = Tables::new(&back, 300);
        let mut d = Decoder::new(&st);
        for &(t, s) in &syms {
            assert_eq!(d.get(&tables, t as usize), s as u32);
        }
        assert!(d.finished());
    }

    #[test]
    fn bits() {
        let mut w = BitW::default();
        for i in 0..1000u32 {
            w.bits(i % 17, i.wrapping_mul(2654435761));
            w.gamma(i);
        }
        let b = w.finish();
        let mut r = BitR::new(&b);
        for i in 0..1000u32 {
            let k = i % 17;
            assert_eq!(r.bits(k), (i.wrapping_mul(2654435761)) & ((1u64 << k) - 1) as u32);
            assert_eq!(r.gamma().unwrap(), i);
        }
        assert!(!r.overrun());
    }
}
