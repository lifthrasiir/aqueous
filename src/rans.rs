//! Static-model rANS: per-context frequency tables counted in a first pass, stored with
//! the data, and four interleaved 32-bit states with 16-bit renormalisation.
//!
//! Adapted from png-recons. Probabilities have 11 bits (rather than 12) so that the decoding
//! tables of a chunk's contexts mostly fit in the L1 cache: the model's next context depends
//! on the symbol just decoded, so table lookups are on the critical path.

use crate::psd::{Error, Result};

fn corrupt() -> Error {
    Error("corrupt entropy coded data".into())
}

pub const PROB_BITS: u32 = 11;
pub const PROB_SCALE: u32 = 1 << PROB_BITS;
/// States stay below 2^31, where the reciprocal division of the encoder is exact.
const RANS_L: u32 = 1 << 15;

/// Frequencies are stored as quantised logarithms, `STEPS` per octave; the weight of level
/// `l` (1..) is about `2^(l / STEPS)`. Level 0 means the symbol is absent.
const STEPS: u32 = 4;
const LEVELS: usize = 64;

/// Integer weights of the levels: round(2^((l - 1) / STEPS) * 256), computed exactly once at
/// compile time from a table of 2^(k/4) in fixed point, so that both sides agree bit for bit.
static WEIGHT: [u64; LEVELS] = {
    // 2^(k/4) * 2^16 for k = 0..4.
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

/// The level of `count` in a table whose largest count is `max`: the largest maps to the
/// top level, and ratios beyond what `PROB_SCALE` resolves are clamped.
fn level_of(count: u32, max: u32) -> u8 {
    if count == 0 {
        return 0;
    }
    let l = (LEVELS - 1) as f64 + (STEPS as f64 * (count as f64 / max as f64).log2()).round();
    l.max(1.0) as u8
}

/// Normalised frequencies (summing to `PROB_SCALE`) from quantised levels (both sides).
fn freqs_from_levels(levels: &[u8]) -> Result<Vec<u32>> {
    let n = levels.len();
    let nz = levels.iter().filter(|&&l| l > 0).count();
    let mut f = vec![0u32; n];
    if nz == 0 {
        // An unused context: any valid table will do.
        f[0] = PROB_SCALE;
        return Ok(f);
    }
    if nz > PROB_SCALE as usize {
        return Err(corrupt());
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
                return Err(corrupt());
            }
            f[m] -= d;
            tot -= d;
        }
    }
    Ok(f)
}

/// Writes the levels of `counts` (a used context) and returns the frequencies both sides
/// will use.
pub fn write_table(w: &mut BitW, counts: &[u32]) -> Vec<u32> {
    let n = counts.len();
    let max = counts.iter().copied().max().unwrap_or(0);
    let levels: Vec<u8> = counts.iter().map(|&c| level_of(c, max)).collect();
    // Each level as the zigzag difference from the previous one, with an Elias-gamma code;
    // zeros (absent symbols) are runs.
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
        // 0 is reserved for runs of absent symbols.
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
                return Err(corrupt());
            }
            i += run;
            continue;
        }
        let z = z - 1;
        let d = if z & 1 == 1 { (z as i32 + 1) / 2 } else { -(z as i32 / 2) };
        let l = prev + d;
        if !(1..LEVELS as i32).contains(&l) {
            return Err(corrupt());
        }
        levels[i] = l as u8;
        prev = l;
        i += 1;
    }
    freqs_from_levels(&levels)
}

/// Estimated bits to code `counts` with its own table (data only).
pub fn cost(counts: &[u32]) -> f64 {
    let tot: f64 = counts.iter().map(|&c| c as f64).sum();
    counts.iter().filter(|&&c| c > 0).map(|&c| c as f64 * (tot / c as f64).log2()).sum()
}

/// Decoding tables of all contexts, flat: per table the symbol of every slot, and per
/// symbol its frequency and cumulative frequency.
pub struct Tables {
    pub lut: Vec<u8>,
    pub fc: Vec<u32>,
}

impl Tables {
    pub fn new(freqs: &[Vec<u32>]) -> Tables {
        let mut lut = vec![0u8; freqs.len() * PROB_SCALE as usize];
        let mut fc = vec![0u32; freqs.len() * 256];
        for (t, f) in freqs.iter().enumerate() {
            let l = &mut lut[t * PROB_SCALE as usize..];
            let mut acc = 0u32;
            for (s, &fs) in f.iter().enumerate() {
                fc[t * 256 + s] = fs | acc << 16;
                l[acc as usize..(acc + fs) as usize].fill(s as u8);
                acc += fs;
            }
        }
        Tables { lut, fc }
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

/// Encodes one symbol into state `x`, writing a renormalisation word below `pos` in `out`
/// (branch-free: the word is stored either way).
#[inline(always)]
fn put(x: &mut u32, e: u64, out: &mut [u8], pos: &mut usize) {
    let freq = ((e >> 45) & 0x1fff) as u32;
    let bias = ((e >> 32) & 0x1fff) as u32;
    let shift = (e >> 58) as u32;
    let rcp = e as u32;
    // States stay below 2^31; one 16-bit renormalisation step is always enough.
    let renorm = *x >= freq << (31 - PROB_BITS);
    out[*pos - 2..*pos].copy_from_slice(&(*x as u16).to_le_bytes());
    *pos -= 2 * renorm as usize;
    let y = if renorm { *x >> 16 } else { *x };
    let q = ((y as u64 * rcp as u64) >> 32) as u32 >> shift;
    *x = y.wrapping_add(bias).wrapping_add(q.wrapping_mul(PROB_SCALE - freq));
}

/// Encodes `syms` (table index, symbol), in order, with interleaved states; returns the
/// byte stream the decoder reads forward. `freqs` are the tables' frequencies.
pub fn encode(syms: &[(u16, u16)], freqs: &[Vec<u32>]) -> Vec<u8> {
    let mut enc = vec![0u64; freqs.len() * 256];
    for (t, f) in freqs.iter().enumerate() {
        let mut acc = 0;
        for (s, &fs) in f.iter().enumerate() {
            if fs > 0 {
                enc[t * 256 + s] = enc_sym(acc, fs);
            }
            acc += fs;
        }
    }
    // Written backwards from the end: at most one word per symbol, the final states, and
    // the slack of the branch-free store.
    let mut out = vec![0u8; 2 * syms.len() + 32];
    let mut pos = out.len();
    let mut x = [RANS_L; 4];
    let e = |(t, s): (u16, u16)| enc[t as usize * 256 + s as usize];
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
    // The decoder reads the states first, high half first.
    for x in [x3, x2, x1, x0] {
        pos -= 4;
        out[pos..pos + 2].copy_from_slice(&((x >> 16) as u16).to_le_bytes());
        out[pos + 2..pos + 4].copy_from_slice(&(x as u16).to_le_bytes());
    }
    out.drain(..pos);
    out
}

/// The decoder: reads the stream forward. Reading past the end yields zeros (the caller
/// checks consistency at the end with [`Decoder::finished`]).
#[derive(Clone, Copy)]
pub struct Decoder<'a> {
    /// The state of the next symbol first.
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

    /// Renormalises `x` (branch-free: the next word is read either way) and makes it the
    /// last state.
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

    /// Decodes a symbol of table `t`.
    #[inline(always)]
    pub fn get(&mut self, tb: &Tables, t: usize) -> u32 {
        let x = self.st[0];
        let slot = x & (PROB_SCALE - 1);
        debug_assert!(t * 256 < tb.fc.len());
        // SAFETY: `t` is a table index of the model (never read from the input), `slot` is
        // below `PROB_SCALE` and `s` below 256.
        let (s, e) = unsafe {
            let s = *tb.lut.get_unchecked(t * PROB_SCALE as usize + slot as usize) as usize;
            (s, *tb.fc.get_unchecked(t * 256 + s))
        };
        self.advance((e & 0xffff) * (x >> PROB_BITS) + slot - (e >> 16));
        s as u32
    }

    /// Decodes a symbol of a binary table `t`.
    #[inline(always)]
    pub fn bit(&mut self, tb: &Tables, t: usize) -> u32 {
        let x = self.st[0];
        let slot = x & (PROB_SCALE - 1);
        debug_assert!(t * 256 < tb.fc.len());
        // SAFETY: as in `get`.
        let f0 = unsafe { *tb.fc.get_unchecked(t * 256) } & 0xffff;
        let b = (slot >= f0) as u32;
        let (lo, f) = if b == 1 { (f0, PROB_SCALE - f0) } else { (0, f0) };
        self.advance(f * (x >> PROB_BITS) + slot - lo);
        b
    }

    /// Whether the whole stream was consumed exactly.
    pub fn finished(&self) -> bool {
        self.pos == self.data.len() && self.st.iter().all(|&x| x == RANS_L)
    }
}

/// LSB-first bit writer for side information and raw bits.
#[derive(Default)]
pub struct BitW {
    pub out: Vec<u8>,
    acc: u64,
    n: u32,
}

impl BitW {
    #[inline(always)]
    pub fn bits(&mut self, k: u32, v: u32) {
        debug_assert!(k <= 32 && (k == 32 || v >> k == 0));
        self.acc |= (v as u64) << self.n;
        self.n += k;
        if self.n >= 32 {
            self.out.extend_from_slice(&(self.acc as u32).to_le_bytes());
            self.acc >>= 32;
            self.n -= 32;
        }
    }
    pub fn gamma(&mut self, v: u32) {
        let v = v as u64 + 1;
        let k = 63 - v.leading_zeros();
        self.bits(k, 0);
        self.bits(1, 1);
        if k > 0 {
            self.bits(k, (v & ((1 << k) - 1)) as u32);
        }
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
#[derive(Clone, Copy)]
pub struct BitR<'a> {
    data: &'a [u8],
    pos: usize,
    acc: u64,
    n: u32,
    over: bool,
}

impl<'a> BitR<'a> {
    pub fn new(data: &'a [u8]) -> BitR<'a> {
        BitR { data, pos: 0, acc: 0, n: 0, over: false }
    }
    #[inline(always)]
    fn refill(&mut self) {
        if let Some(b) = self.data.get(self.pos..self.pos + 8) {
            self.acc |= u64::from_le_bytes(b.try_into().unwrap()) << self.n;
            let k = (63 - self.n) / 8;
            self.pos += k as usize;
            self.n += k * 8;
        } else {
            while self.n <= 56 {
                match self.data.get(self.pos) {
                    Some(&b) => self.acc |= (b as u64) << self.n,
                    None => self.over = self.pos >= self.data.len() + 8,
                }
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
        let mut k = 0;
        while self.bits(1) == 0 {
            k += 1;
            if k > 32 || self.over {
                return Err(corrupt());
            }
        }
        let v = if k > 0 { (1u64 << k) | self.bits(k) as u64 } else { 1 };
        Ok((v - 1) as u32)
    }
    pub fn overrun(&self) -> bool {
        self.over
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let mut seed = 1u64;
        let mut rnd = || {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (seed >> 33) as u32
        };
        let sizes = [256usize, 2, 30, 5];
        let mut counts: Vec<Vec<u32>> = sizes.iter().map(|&n| vec![0; n]).collect();
        let mut syms = vec![];
        for _ in 0..100000 {
            let t = (rnd() % 4) as usize;
            let n = sizes[t] as u32;
            // Table 3 has a single symbol (frequency PROB_SCALE).
            let s = if t == 3 { 2 } else { (rnd() % n).min(rnd() % n).min(rnd() % n) };
            counts[t][s as usize] += 1;
            syms.push((t as u16, s as u16));
        }
        let mut w = BitW::default();
        let freqs: Vec<Vec<u32>> = counts.iter().map(|c| write_table(&mut w, c)).collect();
        let tb = w.finish();
        let mut r = BitR::new(&tb);
        let rf: Vec<Vec<u32>> = sizes.iter().map(|&n| read_table(&mut r, n).unwrap()).collect();
        assert_eq!(freqs, rf);
        let tables = Tables::new(&rf);
        let bytes = encode(&syms, &freqs);
        let mut d = Decoder::new(&bytes);
        for &(t, s) in &syms {
            let v = if t == 1 { d.bit(&tables, 1) } else { d.get(&tables, t as usize) };
            assert_eq!(v, s as u32);
        }
        assert!(d.finished());
    }
}
