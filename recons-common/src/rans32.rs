//! Static-model rANS: per-table frequencies counted in a first pass and stored with the
//! data as quantised logarithms, four interleaved 32-bit states with 16-bit
//! renormalisation, byte output. `BITS` (at most 12) is the probability resolution; the
//! symbols of the decoding tables are `u8` (alphabets of up to 256, smaller tables) or `u16`.
//!
//! Also the LSB-first bit I/O the tables are written with ([`BitW`], [`BitR`]); `MSB` picks
//! the bit order of the fields of their Elias gamma codes.

use crate::Corrupt;

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

const fn check_bits(bits: u32) {
    assert!(bits >= 1 && bits <= 12, "frequencies and cumulative frequencies are packed in 13 bits");
}

/// The level of `count` in a table whose largest count is `max`: the largest maps to the
/// top level, and ratios beyond what the resolution resolves are clamped.
fn level_of(count: u32, max: u32) -> u8 {
    if count == 0 {
        return 0;
    }
    let l = (LEVELS - 1) as f64 + (STEPS as f64 * (count as f64 / max as f64).log2()).round();
    l.max(1.0) as u8
}

/// Normalised frequencies (summing to `1 << BITS`) from quantised levels (both sides).
fn freqs_from_levels<const BITS: u32>(levels: &[u8]) -> Result<Vec<u32>, Corrupt> {
    const { check_bits(BITS) };
    let scale = 1u32 << BITS;
    let n = levels.len();
    let nz = levels.iter().filter(|&&l| l > 0).count();
    let mut f = vec![0u32; n];
    if nz == 0 {
        // An unused table: any valid one will do.
        f[0] = scale;
        return Ok(f);
    }
    if nz > scale as usize {
        return Err(Corrupt("too many symbols for the probability resolution"));
    }
    let sum: u64 = levels.iter().map(|&l| WEIGHT[l as usize]).sum();
    let mut tot = 0u32;
    for i in 0..n {
        if levels[i] > 0 {
            f[i] = ((WEIGHT[levels[i] as usize] * scale as u64 / sum) as u32).max(1);
            tot += f[i];
        }
    }
    // Fix the rounding on the largest frequencies (first index wins ties).
    while tot != scale {
        let m = (0..n).max_by_key(|&i| (f[i], std::cmp::Reverse(i))).unwrap();
        if tot < scale {
            f[m] += scale - tot;
            tot = scale;
        } else {
            let d = (tot - scale).min(f[m] - 1);
            if d == 0 {
                return Err(Corrupt("unnormalisable table"));
            }
            f[m] -= d;
            tot -= d;
        }
    }
    Ok(f)
}

/// Writes the levels of `counts` (a used table) and returns the frequencies both sides
/// will use.
pub fn write_table<const BITS: u32, const MSB: bool>(w: &mut BitW<MSB>, counts: &[u32]) -> Vec<u32> {
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
    freqs_from_levels::<BITS>(&levels).expect("valid levels")
}

/// Reads a table of `n` symbols written by [`write_table`].
pub fn read_table<const BITS: u32, const MSB: bool>(r: &mut BitR<MSB>, n: usize) -> Result<Vec<u32>, Corrupt> {
    let mut levels = vec![0u8; n];
    let mut prev = 0i32;
    let mut i = 0;
    while i < n {
        let z = r.gamma()?;
        if z == 0 {
            let run = r.gamma()? as usize + 1;
            if i + run > n {
                return Err(Corrupt("bad table"));
            }
            i += run;
            continue;
        }
        let z = z - 1;
        let d = if z & 1 == 1 { (z as i32 + 1) / 2 } else { -(z as i32 / 2) };
        let l = prev + d;
        if !(1..LEVELS as i32).contains(&l) {
            return Err(Corrupt("bad table"));
        }
        levels[i] = l as u8;
        prev = l;
        i += 1;
    }
    freqs_from_levels::<BITS>(&levels)
}

/// Estimated bits to code `counts` with its own exact table (data only).
pub fn cost(counts: &[u32]) -> f64 {
    let tot: f64 = counts.iter().map(|&c| c as f64).sum();
    counts.iter().filter(|&&c| c > 0).map(|&c| c as f64 * (tot / c as f64).log2()).sum()
}

/// Bits to code `counts` with the frequencies `f`.
pub fn cost_with<const BITS: u32>(counts: &[u32], f: &[u32]) -> f64 {
    counts
        .iter()
        .zip(f)
        .filter(|&(&c, _)| c > 0)
        .map(|(&c, &q)| c as f64 * (BITS as f64 - (q.max(1) as f64).log2()))
        .sum()
}

/// A symbol in the decoding tables.
pub trait Symbol: Copy + Default {
    /// Alphabet size limit.
    const MAX: usize;
    /// log2 of the per-table stride if fixed (the full alphabet, for small symbols: a
    /// constant shift in the decoder).
    const STRIDE_LOG: Option<u32>;
    fn new(s: usize) -> Self;
    fn get(self) -> usize;
}

impl Symbol for u8 {
    const MAX: usize = 1 << 8;
    const STRIDE_LOG: Option<u32> = Some(8);
    #[inline(always)]
    fn new(s: usize) -> Self {
        s as u8
    }
    #[inline(always)]
    fn get(self) -> usize {
        self as usize
    }
}

impl Symbol for u16 {
    const MAX: usize = 1 << 16;
    const STRIDE_LOG: Option<u32> = None;
    #[inline(always)]
    fn new(s: usize) -> Self {
        s as u16
    }
    #[inline(always)]
    fn get(self) -> usize {
        self as usize
    }
}

/// log2 of the per-table stride of the flat symbol arrays: the largest alphabet, rounded
/// up to a power of two.
fn stride_log(freqs: &[Vec<u32>]) -> u32 {
    freqs.iter().map(|f| f.len()).max().unwrap_or(1).next_power_of_two().trailing_zeros()
}

/// Decoding tables, flat: per table the symbol of every slot, and per symbol its frequency
/// and cumulative frequency.
pub struct Tables<S: Symbol, const BITS: u32> {
    lut: Vec<S>,
    fc: Vec<u32>,
    /// log2 of the symbols per table in `fc`.
    shift: u32,
    n: usize,
}

impl<S: Symbol, const BITS: u32> Tables<S, BITS> {
    /// `freqs[t]` must sum to `1 << BITS` (from [`write_table`] or [`read_table`]).
    pub fn new(freqs: &[Vec<u32>]) -> Self {
        const { check_bits(BITS) };
        let scale = 1usize << BITS;
        let shift = S::STRIDE_LOG.unwrap_or_else(|| stride_log(freqs));
        let mut lut = vec![S::default(); freqs.len() * scale];
        let mut fc = vec![0u32; freqs.len() << shift];
        for (t, f) in freqs.iter().enumerate() {
            assert!(f.len() <= S::MAX);
            let l = &mut lut[t * scale..];
            let mut acc = 0u32;
            for (s, &fs) in f.iter().enumerate() {
                fc[(t << shift) + s] = fs | acc << 16;
                l[acc as usize..(acc + fs) as usize].fill(S::new(s));
                acc += fs;
            }
        }
        Tables { lut, fc, shift, n: freqs.len() }
    }

    /// The number of tables.
    pub fn len(&self) -> usize {
        self.n
    }

    pub fn is_empty(&self) -> bool {
        self.n == 0
    }

    /// Index of table `t` in `fc`.
    #[inline(always)]
    fn base(&self, t: usize) -> usize {
        t << S::STRIDE_LOG.unwrap_or(self.shift)
    }
}

/// A symbol's encoding parameters, packed: the reciprocal of its frequency (after
/// ryg_rans, division by a multiplication), the bias, the frequency and the shift.
#[inline(always)]
fn enc_sym<const BITS: u32>(start: u32, freq: u32) -> u64 {
    let (rcp, shift, bias) = if freq < 2 {
        (!0u32, 0u32, start + (1 << BITS) - 1)
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
fn put<const BITS: u32>(x: &mut u32, e: u64, out: &mut [u8], pos: &mut usize) {
    let freq = ((e >> 45) & 0x1fff) as u32;
    let bias = ((e >> 32) & 0x1fff) as u32;
    let shift = (e >> 58) as u32;
    let rcp = e as u32;
    // States stay below 2^31; one 16-bit renormalisation step is always enough.
    let renorm = *x >= freq << (31 - BITS);
    out[*pos - 2..*pos].copy_from_slice(&(*x as u16).to_le_bytes());
    *pos -= 2 * renorm as usize;
    let y = if renorm { *x >> 16 } else { *x };
    let q = ((y as u64 * rcp as u64) >> 32) as u32 >> shift;
    *x = y.wrapping_add(bias).wrapping_add(q.wrapping_mul((1 << BITS) - freq));
}

/// Encodes `syms` (table index, symbol), in order, with interleaved states; returns the
/// byte stream the decoder reads forward. `freqs` are the tables' frequencies.
pub fn encode<const BITS: u32>(syms: &[(u16, u16)], freqs: &[Vec<u32>]) -> Vec<u8> {
    const { check_bits(BITS) };
    let shift = stride_log(freqs);
    let mut enc = vec![0u64; freqs.len() << shift];
    for (t, f) in freqs.iter().enumerate() {
        let mut acc = 0;
        for (s, &fs) in f.iter().enumerate() {
            if fs > 0 {
                enc[(t << shift) + s] = enc_sym::<BITS>(acc, fs);
            }
            acc += fs;
        }
    }
    // Written backwards from the end: at most one word per symbol, the final states, and
    // the slack of the branch-free store.
    let mut out = vec![0u8; 2 * syms.len() + 32];
    let mut pos = out.len();
    let mut x = [RANS_L; 4];
    let e = |(t, s): (u16, u16)| enc[((t as usize) << shift) + s as usize];
    let mut i = syms.len();
    while !i.is_multiple_of(4) {
        i -= 1;
        put::<BITS>(&mut x[i & 3], e(syms[i]), &mut out, &mut pos);
    }
    let [mut x0, mut x1, mut x2, mut x3] = x;
    while i > 0 {
        i -= 4;
        put::<BITS>(&mut x3, e(syms[i + 3]), &mut out, &mut pos);
        put::<BITS>(&mut x2, e(syms[i + 2]), &mut out, &mut pos);
        put::<BITS>(&mut x1, e(syms[i + 1]), &mut out, &mut pos);
        put::<BITS>(&mut x0, e(syms[i]), &mut out, &mut pos);
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

    /// Decodes a symbol of table `t` (below `tb.len()`; a table index of the model, never
    /// read from the input).
    #[inline(always)]
    pub fn get<S: Symbol, const BITS: u32>(&mut self, tb: &Tables<S, BITS>, t: usize) -> u32 {
        let x = self.st[0];
        let slot = x & ((1 << BITS) - 1);
        debug_assert!(t < tb.n);
        // SAFETY: `t` is below the number of tables, `slot` below `1 << BITS`, and every
        // symbol in the LUT below the stride.
        let (s, e) = unsafe {
            let s = tb.lut.get_unchecked((t << BITS) + slot as usize).get();
            (s, *tb.fc.get_unchecked(tb.base(t) + s))
        };
        self.advance((e & 0xffff) * (x >> BITS) + slot - (e >> 16));
        s as u32
    }

    /// Decodes a symbol of a binary table `t`.
    #[inline(always)]
    pub fn bit<S: Symbol, const BITS: u32>(&mut self, tb: &Tables<S, BITS>, t: usize) -> u32 {
        let x = self.st[0];
        let slot = x & ((1 << BITS) - 1);
        debug_assert!(t < tb.n);
        // SAFETY: as in `get`.
        let f0 = unsafe { *tb.fc.get_unchecked(tb.base(t)) } & 0xffff;
        let b = (slot >= f0) as u32;
        let (lo, f) = if b == 1 { (f0, (1 << BITS) - f0) } else { (0, f0) };
        self.advance(f * (x >> BITS) + slot - lo);
        b
    }

    /// Whether the whole stream was consumed exactly.
    pub fn finished(&self) -> bool {
        self.pos == self.data.len() && self.st.iter().all(|&x| x == RANS_L)
    }
}

/// Reverses the low `k` bits of `v` (`k` <= 32).
#[inline(always)]
fn rev(v: u64, k: u32) -> u32 {
    if k == 0 { 0 } else { (v.reverse_bits() >> (64 - k)) as u32 }
}

/// LSB-first bit writer.
#[derive(Default)]
pub struct BitW<const MSB: bool = false> {
    pub out: Vec<u8>,
    acc: u64,
    n: u32,
}

impl<const MSB: bool> BitW<MSB> {
    /// Writes the low `k` bits of `v` (`k` <= 32).
    #[inline(always)]
    pub fn bits(&mut self, k: u32, v: u32) {
        debug_assert!(k <= 32);
        self.acc |= (v as u64 & ((1u64 << k) - 1)) << self.n;
        self.n += k;
        if self.n >= 32 {
            self.out.extend_from_slice(&(self.acc as u32).to_le_bytes());
            self.acc >>= 32;
            self.n -= 32;
        }
    }

    /// Elias gamma code of `v + 1`: the bit length less one as zeros, a one, then the bits
    /// below the leading one (a little-endian field, or most significant first with `MSB`).
    pub fn gamma(&mut self, v: u32) {
        let v = v as u64 + 1;
        let k = 63 - v.leading_zeros();
        self.bits(k, 0);
        self.bits(1, 1);
        self.bits(k, if MSB { rev(v, k) } else { v as u32 });
    }

    /// Number of bits written so far.
    pub fn bit_len(&self) -> usize {
        self.out.len() * 8 + self.n as usize
    }

    /// Pads with zero bits to a byte boundary and returns the bytes.
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
pub struct BitR<'a, const MSB: bool = false> {
    data: &'a [u8],
    pos: usize,
    acc: u64,
    n: u32,
    /// Bytes fetched beyond the data.
    over: usize,
}

impl<'a, const MSB: bool> BitR<'a, MSB> {
    pub fn new(data: &'a [u8]) -> Self {
        BitR { data, pos: 0, acc: 0, n: 0, over: 0 }
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
                    None => self.over += 1,
                }
                self.pos += 1;
                self.n += 8;
            }
        }
    }

    /// Reads `k` bits (`k` <= 32).
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

    /// Reads a [`BitW::gamma`] code.
    pub fn gamma(&mut self) -> Result<u32, Corrupt> {
        let mut k = 0;
        while self.bits(1) == 0 {
            k += 1;
            if k > 32 || self.overrun() {
                return Err(Corrupt("bad gamma code"));
            }
        }
        let f = self.bits(k);
        let v = (1u64 << k) | if MSB { rev(f as u64, k) } else { f } as u64;
        u32::try_from(v - 1).map_err(|_| Corrupt("bad gamma code"))
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

    fn roundtrip<S: Symbol, const BITS: u32, const MSB: bool>(sizes: &[usize]) {
        let mut seed = 1u64;
        let mut rnd = || {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (seed >> 33) as u32
        };
        let mut counts: Vec<Vec<u32>> = sizes.iter().map(|&n| vec![0; n]).collect();
        let mut syms = vec![];
        for _ in 0..100000 {
            let t = rnd() as usize % sizes.len();
            let n = sizes[t] as u32;
            // Table 3 has a single symbol (frequency 1 << BITS).
            let s = if t == 3 { 2 } else { (rnd() % n).min(rnd() % n).min(rnd() % n) };
            counts[t][s as usize] += 1;
            syms.push((t as u16, s as u16));
        }
        let mut w = BitW::<MSB>::default();
        let freqs: Vec<Vec<u32>> = counts.iter().map(|c| write_table::<BITS, MSB>(&mut w, c)).collect();
        let tb = w.finish();
        let mut r = BitR::<MSB>::new(&tb);
        let rf: Vec<Vec<u32>> = sizes.iter().map(|&n| read_table::<BITS, MSB>(&mut r, n).unwrap()).collect();
        assert!(!r.overrun());
        assert_eq!(freqs, rf);
        let tables = Tables::<S, BITS>::new(&rf);
        let bytes = encode::<BITS>(&syms, &freqs);
        let mut d = Decoder::new(&bytes);
        for &(t, s) in &syms {
            let v = if t == 1 { d.bit(&tables, 1) } else { d.get(&tables, t as usize) };
            assert_eq!(v, s as u32);
        }
        assert!(d.finished());
    }

    #[test]
    fn roundtrip_u8_12() {
        roundtrip::<u8, 12, false>(&[256, 2, 30, 5]);
    }

    #[test]
    fn roundtrip_u8_11() {
        roundtrip::<u8, 11, false>(&[256, 2, 30, 5, 256]);
    }

    #[test]
    fn roundtrip_u16_12() {
        roundtrip::<u16, 12, true>(&[300, 2, 288, 5, 30]);
    }

    fn bits<const MSB: bool>() {
        let mut w = BitW::<MSB>::default();
        for i in 0..1000u32 {
            w.bits(i % 17, i.wrapping_mul(2654435761));
            w.gamma(i.wrapping_mul(i).wrapping_mul(977));
        }
        w.gamma(u32::MAX);
        let len = w.bit_len();
        let b = w.finish();
        assert_eq!(b.len(), len.div_ceil(8));
        let mut r = BitR::<MSB>::new(&b);
        for i in 0..1000u32 {
            let k = i % 17;
            assert_eq!(r.bits(k), i.wrapping_mul(2654435761) & ((1u64 << k) - 1) as u32);
            assert_eq!(r.gamma(), Ok(i.wrapping_mul(i).wrapping_mul(977)));
        }
        assert_eq!(r.gamma(), Ok(u32::MAX));
        assert!(!r.overrun());
        r.bits(8);
        assert!(r.overrun());
    }

    #[test]
    fn bits_lsb() {
        bits::<false>();
    }

    #[test]
    fn bits_msb() {
        bits::<true>();
    }

    /// The fields of the gamma codes: 5 + 1 = 0b110, two zeros, the leading one, then 0b10.
    #[test]
    fn gamma_layout() {
        let mut w = BitW::<false>::default();
        w.gamma(5);
        assert_eq!(w.finish(), [0b10100]);
        let mut w = BitW::<true>::default();
        w.gamma(5);
        assert_eq!(w.finish(), [0b01100]);
    }
}
