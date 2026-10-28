//! Static (non-adaptive) codes for the correction streams: Elias gamma for headers, and
//! per-stream symbol frequencies with rANS for the fields of corrections.
//!
//! A field's values are split into a bucket symbol (entropy coded with the field's fixed
//! frequencies) and raw extra bits, like DEFLATE's length and distance codes: values below
//! `DIRECT` are their own symbol, larger ones take one symbol per half-octave. Frequencies
//! are sent rounded to half-octaves; nothing adapts while coding.

use crate::bits::{BitReader, BitWriter};
use crate::{Error, Result};
use aqr_common::varint;

/// Values below this are coded as a single symbol.
const DIRECT: u32 = 16;
/// Bucket symbols per field: the direct values, then two per bit length from 5 to 32.
pub const ALPHABET: usize = DIRECT as usize + 2 * 28;
/// Frequencies are scaled to sum to 2^PROB_BITS.
const PROB_BITS: u32 = 15;
const TOTAL: u32 = 1 << PROB_BITS;
/// Lower bound of the rANS state.
const RANS_L: u32 = 1 << 23;

/// Bucket symbol, number of extra bits and their value for `v`.
#[inline]
pub fn bucket(v: u32) -> (usize, u32, u32) {
    if v < DIRECT {
        return (v as usize, 0, 0);
    }
    let nb = 32 - v.leading_zeros(); // >= 5
    let second = (v >> (nb - 2)) & 1;
    let extra = nb - 2;
    (
        DIRECT as usize + (nb as usize - 5) * 2 + second as usize,
        extra,
        v & ((1 << extra) - 1),
    )
}

/// Inverse of [`bucket`]: the value of symbol `sym` with extra bits `x`.
#[inline]
pub fn unbucket(sym: usize, x: u32) -> u32 {
    if sym < DIRECT as usize {
        return sym as u32;
    }
    let k = sym - DIRECT as usize;
    let nb = (k / 2 + 5) as u32;
    let top = 0b10 | (k as u32 & 1);
    top << (nb - 2) | x
}

#[inline]
pub fn extra_bits(sym: usize) -> u32 {
    if sym < DIRECT as usize {
        0
    } else {
        ((sym - DIRECT as usize) / 2 + 3) as u32
    }
}

pub fn put_gamma(w: &mut BitWriter, v: u64) {
    let v1 = v + 1;
    let nb = 63 - v1.leading_zeros();
    for _ in 0..nb {
        w.bits(1, 1);
    }
    w.bits(1, 0);
    put_bits64(w, nb, v1 & !(1 << nb));
}

pub fn get_gamma(r: &mut BitReader) -> Result<u64> {
    let mut nb = 0;
    while r.bits(1)? == 1 {
        nb += 1;
        if nb > 63 {
            return Err(Error::Corrupt);
        }
    }
    let rest = get_bits64(r, nb)?;
    Ok(((1u64 << nb) | rest) - 1)
}

pub fn put_bits64(w: &mut BitWriter, n: u32, v: u64) {
    if n > 32 {
        w.bits(32, v as u32);
        w.bits(n - 32, (v >> 32) as u32);
    } else {
        w.bits(n, v as u32);
    }
}

pub fn get_bits64(r: &mut BitReader, n: u32) -> Result<u64> {
    if n > 32 {
        let lo = r.bits(32)? as u64;
        Ok(lo | (r.bits(n - 32)? as u64) << 32)
    } else {
        Ok(r.bits(n)? as u64)
    }
}

/// Gamma code of a value that must fit in `usize` and not exceed `max`.
pub fn get_usize(r: &mut BitReader, max: usize) -> Result<usize> {
    let v = get_gamma(r)?;
    if v > max as u64 {
        return Err(Error::Corrupt);
    }
    Ok(v as usize)
}

pub fn put_varint(out: &mut Vec<u8>, v: u64) {
    varint::put(out, v)
}

pub fn get_varint(input: &[u8], at: &mut usize) -> Result<u64> {
    varint::get(input, at).map_err(|_| Error::Corrupt)
}

/// Symbol counts of one field.
#[derive(Clone)]
pub struct Counts(pub [u32; ALPHABET]);

impl Default for Counts {
    fn default() -> Self {
        Counts([0; ALPHABET])
    }
}

impl Counts {
    #[inline]
    pub fn add(&mut self, v: u32) {
        self.0[bucket(v).0] += 1;
    }

    pub fn merge(&mut self, o: &Counts) {
        for (a, b) in self.0.iter_mut().zip(&o.0) {
            *a += b;
        }
    }

    /// Estimated cost in bits of coding these values with their own frequencies, table
    /// included.
    pub fn cost(&self) -> f64 {
        let n: u64 = self.0.iter().map(|&c| c as u64).sum();
        let mut bits = 1.0;
        let mut last = 0;
        for (s, &c) in self.0.iter().enumerate() {
            if c > 0 {
                last = s;
                let p = c as f64 / n as f64;
                bits += c as f64 * (-p.log2() + extra_bits(s) as f64) + 6.0;
            }
        }
        if n > 0 {
            bits += 7.0 + last as f64;
        }
        bits
    }
}

/// Weight of a frequency sent as `q`: about 2^(q/2).
fn weight(q: u8) -> u64 {
    (if q & 1 == 1 { 181 } else { 128 }) << (q / 2)
}

/// Fixed symbol frequencies of one field, summing to `TOTAL` (or all zero if the field
/// is unused).
#[derive(Clone)]
pub struct Model {
    freq: [u32; ALPHABET],
    cum: [u32; ALPHABET + 1],
    /// The rounded frequencies sent, by symbol (0: unused).
    q: [u8; ALPHABET],
    /// The symbol of slot `i << LUT_SHIFT`, where decoding starts looking (or zeros).
    lut: [u8; TOTAL as usize >> LUT_SHIFT],
    /// Per symbol, how the encoder divides by its frequency (see [`Enc`]).
    enc: [Enc; ALPHABET],
}

/// A symbol as the encoder codes it: division by its frequency as a multiplication by a
/// reciprocal (exact for states below 2^31), after ryg's rans_byte.
#[derive(Clone, Copy, Default)]
struct Enc {
    rcp: u32,
    shift: u32,
    bias: u32,
    /// `TOTAL - freq`.
    cmpl: u32,
    /// Renormalize while the state is at least this.
    x_max: u32,
}

impl Enc {
    fn new(start: u32, freq: u32) -> Enc {
        let x_max = ((RANS_L >> PROB_BITS) << 8) * freq;
        if freq == 1 {
            return Enc {
                rcp: u32::MAX,
                shift: 0,
                bias: start + TOTAL - 1,
                cmpl: TOTAL - 1,
                x_max,
            };
        }
        let shift = 32 - (freq - 1).leading_zeros();
        Enc {
            rcp: (1u64 << (shift + 31)).div_ceil(freq as u64) as u32,
            shift: shift - 1,
            bias: start,
            cmpl: TOTAL - freq,
            x_max,
        }
    }
}

const LUT_SHIFT: u32 = 5;

impl Model {
    pub fn new(c: &Counts) -> Model {
        let mut q = [0u8; ALPHABET];
        for (s, &n) in c.0.iter().enumerate() {
            if n > 0 {
                // 1 + round(2 log2 n), in 1..=64.
                q[s] = 1 + (2.0 * (n as f64).log2()).round().min(62.0) as u8;
            }
        }
        Model::from_q_lut(q, false)
    }

    fn from_q(q: [u8; ALPHABET]) -> Model {
        Model::from_q_lut(q, true)
    }

    /// The model of rounded frequencies `q`; with `with_lut` false, without the decoding table
    /// (decoding still works, more slowly).
    fn from_q_lut(q: [u8; ALPHABET], with_lut: bool) -> Model {
        let mut freq = [0u32; ALPHABET];
        let sum: u64 = q.iter().filter(|&&q| q > 0).map(|&q| weight(q - 1)).sum();
        for s in 0..ALPHABET {
            if q[s] > 0
                && let Some(f) = (weight(q[s] - 1) * TOTAL as u64).checked_div(sum)
            {
                freq[s] = (f as u32).max(1);
            }
        }
        if sum > 0 {
            // Make them sum to TOTAL, on the most frequent symbol.
            let total: u32 = freq.iter().sum();
            let big = (0..ALPHABET)
                .max_by_key(|&s| (freq[s], usize::MAX - s))
                .unwrap();
            freq[big] = freq[big] + TOTAL - total;
        }
        let mut cum = [0u32; ALPHABET + 1];
        for s in 0..ALPHABET {
            cum[s + 1] = cum[s] + freq[s];
        }
        let mut lut = [0u8; TOTAL as usize >> LUT_SHIFT];
        if with_lut {
            let mut sym = 0;
            for (i, l) in lut.iter_mut().enumerate() {
                let slot = (i as u32) << LUT_SHIFT;
                while sym + 1 < ALPHABET && cum[sym + 1] <= slot {
                    sym += 1;
                }
                *l = sym as u8;
            }
        }
        let mut enc = [Enc::default(); ALPHABET];
        for s in 0..ALPHABET {
            if freq[s] > 0 {
                enc[s] = Enc::new(cum[s], freq[s]);
            }
        }
        Model {
            freq,
            cum,
            q,
            lut,
            enc,
        }
    }

    /// The rounded frequencies: whether there are any, the last symbol used, then for every
    /// symbol up to it whether it is used and its rounded frequency.
    fn write_table(&self, w: &mut BitWriter) {
        let Some(last) = self.q.iter().rposition(|&q| q > 0) else {
            w.bits(1, 0);
            return;
        };
        w.bits(1, 1);
        w.bits(7, last as u32);
        for &q in &self.q[..=last] {
            if q == 0 {
                w.bits(1, 0);
            } else {
                w.bits(1, 1);
                w.bits(6, q as u32 - 1);
            }
        }
    }

    fn table_bits(&self) -> f64 {
        match self.q.iter().rposition(|&q| q > 0) {
            None => 1.0,
            Some(last) => {
                8.0 + (last + 1) as f64 + 6.0 * self.q.iter().filter(|&&q| q > 0).count() as f64
            }
        }
    }

    /// The model to code `c` with: `default` (one bit) or the counts' own (its table), and
    /// writes that choice.
    pub fn choose(c: &Counts, default: &Model, w: &mut BitWriter) -> Model {
        let own = Model::new(c);
        let n: u32 = c.0.iter().sum();
        if n > 0 && default.bits(c) + 1.0 <= own.bits(c) + own.table_bits() + 1.0 {
            w.bits(1, 1);
            return default.clone();
        }
        w.bits(1, 0);
        own.write_table(w);
        own
    }

    /// Reads what [`Model::choose`] wrote.
    pub fn read(r: &mut BitReader, default: &Model) -> Result<Model> {
        if r.bits(1)? == 1 {
            return Ok(default.clone());
        }
        let mut q = [0u8; ALPHABET];
        if r.bits(1)? == 1 {
            let last = r.bits(7)? as usize;
            if last >= ALPHABET {
                return Err(Error::Corrupt);
            }
            for x in &mut q[..=last] {
                if r.bits(1)? == 1 {
                    *x = r.bits(6)? as u8 + 1;
                }
            }
        }
        Ok(Model::from_q(q))
    }

    /// A default model: the mean of per-stream symbol distributions `p` (summing to 1),
    /// rounded, every symbol possible.
    pub fn from_distribution(p: &[f64]) -> Model {
        let mut q = [0u8; ALPHABET];
        for (x, &p) in q.iter_mut().zip(p) {
            *x = 1
                + (2.0 * (p * (1u64 << 24) as f64).max(1.0).log2())
                    .round()
                    .min(62.0) as u8;
        }
        Model::from_q(q)
    }

    /// The rounded frequencies as text (see [`Model::parse`]).
    pub fn to_text(&self) -> String {
        self.q
            .iter()
            .map(|q| q.to_string())
            .collect::<Vec<_>>()
            .join(" ")
    }

    pub fn parse(s: &str) -> Option<Model> {
        let v: Vec<u8> = s
            .split_whitespace()
            .map(|x| x.parse().ok())
            .collect::<Option<_>>()?;
        Model::from_qs(&v)
    }

    /// The rounded frequencies of a default model, by symbol (1 to 63).
    pub fn qs(&self) -> &[u8; ALPHABET] {
        &self.q
    }

    /// A default model of [`Model::qs`] `q`, if every symbol has one in 1 to 63.
    pub fn from_qs(q: &[u8]) -> Option<Model> {
        let q: [u8; ALPHABET] = q.try_into().ok()?;
        q.iter()
            .all(|q| (1..=63).contains(q))
            .then(|| Model::from_q(q))
    }

    /// Every symbol equally likely.
    pub fn uniform() -> Model {
        Model::from_q([1; ALPHABET])
    }

    /// Bits these counts take with this model (not counting the table).
    pub fn bits(&self, c: &Counts) -> f64 {
        (0..ALPHABET)
            .filter(|&s| c.0[s] > 0)
            .map(|s| {
                c.0[s] as f64 * ((TOTAL as f64 / self.freq[s] as f64).log2() + extra_bits(s) as f64)
            })
            .sum()
    }
}

/// Writes symbols with their models (rANS, coded backwards at the end) and raw bits.
#[derive(Default)]
pub struct Writer {
    /// Each symbol in order.
    syms: Vec<Enc>,
    extra: BitWriter,
}

impl Writer {
    #[inline]
    pub fn put(&mut self, m: &Model, v: u32) {
        let (s, n, x) = bucket(v);
        debug_assert!(m.freq[s] > 0, "symbol {s} not in the model");
        self.syms.push(m.enc[s]);
        self.extra.bits(n, x);
    }

    pub fn raw(&mut self, n: u32, v: u32) {
        self.extra.bits(n, v);
    }

    /// The length of the rANS bytes, the rANS bytes, then the raw bits; nothing if nothing
    /// was written.
    pub fn finish(mut self) -> Vec<u8> {
        self.extra.bits(self.extra.pad_len(), 0);
        let extra = self.extra.finish();
        let mut out = vec![];
        if self.syms.is_empty() {
            if !extra.is_empty() {
                put_varint(&mut out, 0);
                out.extend(extra);
            }
            return out;
        }
        let mut rans = Vec::with_capacity(self.syms.len() / 2 + 8);
        let mut x = RANS_L;
        for e in self.syms.iter().rev() {
            while x >= e.x_max {
                rans.push(x as u8);
                x >>= 8;
            }
            let q = (((x as u64 * e.rcp as u64) >> 32) >> e.shift) as u32;
            x = x + e.bias + q * e.cmpl;
        }
        rans.extend_from_slice(&x.to_le_bytes());
        rans.reverse();
        put_varint(&mut out, rans.len() as u64);
        out.extend(rans);
        out.extend(extra);
        out
    }
}

/// Reads what [`Writer`] wrote.
pub struct Reader<'a> {
    x: u32,
    rans: &'a [u8],
    at: usize,
    extra: Bits<'a>,
}

/// LSB-first bits, reading past the end as an error.
struct Bits<'a> {
    data: &'a [u8],
    /// Next byte to load into `acc`.
    at: usize,
    acc: u64,
    n: u32,
}

impl<'a> Bits<'a> {
    fn new(data: &'a [u8]) -> Self {
        Bits {
            data,
            at: 0,
            acc: 0,
            n: 0,
        }
    }

    #[inline]
    fn get(&mut self, k: u32) -> Result<u32> {
        if k == 0 {
            return Ok(0);
        }
        if self.n < k {
            if let Some(w) = self.data.get(self.at..self.at + 4) {
                self.acc |= (u32::from_le_bytes(w.try_into().unwrap()) as u64) << self.n;
                self.at += 4;
                self.n += 32;
            } else {
                while self.n < k {
                    let b = *self.data.get(self.at).ok_or(Error::Truncated)?;
                    self.acc |= (b as u64) << self.n;
                    self.at += 1;
                    self.n += 8;
                }
            }
        }
        let v = (self.acc & ((1u64 << k) - 1)) as u32;
        self.acc >>= k;
        self.n -= k;
        Ok(v)
    }
}

impl<'a> Reader<'a> {
    pub fn new(body: &'a [u8]) -> Result<Reader<'a>> {
        if body.is_empty() {
            return Ok(Reader {
                x: 0,
                rans: &[],
                at: 0,
                extra: Bits::new(&[]),
            });
        }
        let mut at = 0;
        let n = get_varint(body, &mut at)? as usize;
        let rans = body
            .get(at..at.checked_add(n).ok_or(Error::Corrupt)?)
            .ok_or(Error::Corrupt)?;
        let extra = Bits::new(&body[at + n..]);
        let mut x = 0;
        if n > 0 {
            let head = rans.get(..4).ok_or(Error::Corrupt)?;
            x = u32::from_be_bytes(head.try_into().unwrap());
        }
        Ok(Reader {
            x,
            rans,
            at: 4,
            extra,
        })
    }

    #[inline]
    pub fn get(&mut self, m: &Model) -> Result<u32> {
        if m.cum[ALPHABET] != TOTAL || self.x < RANS_L {
            return Err(Error::Corrupt);
        }
        let slot = self.x & (TOTAL - 1);
        let mut s = m.lut[(slot >> LUT_SHIFT) as usize] as usize;
        while m.cum[s + 1] <= slot {
            s += 1;
        }
        let mut x = m.freq[s] * (self.x >> PROB_BITS) + slot - m.cum[s];
        while x < RANS_L {
            let b = *self.rans.get(self.at).ok_or(Error::Corrupt)?;
            self.at += 1;
            x = x << 8 | b as u32;
        }
        self.x = x;
        let e = self.extra.get(extra_bits(s))?;
        Ok(unbucket(s, e))
    }

    pub fn raw(&mut self, n: u32) -> Result<u32> {
        self.extra.get(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A length of the rANS bytes that overflows is rejected (in debug builds too).
    #[test]
    fn huge_rans_length_is_rejected() {
        let mut body = vec![0xff; 9];
        body.push(0x01);
        assert!(Reader::new(&body).is_err());
    }

    #[test]
    fn buckets_round_trip() {
        for v in (0..5000).chain([65535, 1 << 20, u32::MAX - 1, u32::MAX]) {
            let (s, n, x) = bucket(v);
            assert!(s < ALPHABET);
            assert_eq!(n, extra_bits(s));
            assert_eq!(unbucket(s, x), v, "{v}");
        }
    }

    #[test]
    fn codes_round_trip() {
        let vals: Vec<u32> = (0..3000u32).map(|i| (i * 7919) % (1 + i % 300)).collect();
        let skewed: Vec<u32> = (0..5000u32).map(|i| (i % 97 == 0) as u32).collect();
        for set in [
            &vals[..],
            &skewed[..],
            &[5, 5, 5][..],
            &[][..],
            &[1, 100000][..],
        ] {
            // Two fields interleaved.
            let mut c = [Counts::default(), Counts::default()];
            for (i, &v) in set.iter().enumerate() {
                c[i % 2].add(v);
            }
            let def = Model::uniform();
            let mut w = BitWriter::new();
            let m = c.each_ref().map(|c| Model::choose(c, &def, &mut w));
            w.bits(w.pad_len(), 0);
            let table = w.finish();
            let mut cw = Writer::default();
            for (i, &v) in set.iter().enumerate() {
                cw.put(&m[i % 2], v);
                cw.raw(3, i as u32 & 7);
            }
            let body = cw.finish();
            let mut r = BitReader::new(&table);
            let m = [
                Model::read(&mut r, &def).unwrap(),
                Model::read(&mut r, &def).unwrap(),
            ];
            let mut cr = Reader::new(&body).unwrap();
            for (i, &v) in set.iter().enumerate() {
                assert_eq!(cr.get(&m[i % 2]).unwrap(), v);
                assert_eq!(cr.raw(3).unwrap(), i as u32 & 7);
            }
            if set == &skewed[..] {
                let ideal = c[0].cost() + c[1].cost() + 3.0 * set.len() as f64;
                assert!(
                    (body.len() * 8) as f64 <= ideal * 1.05 + 64.0,
                    "{} vs {ideal}",
                    body.len() * 8
                );
            }
        }
    }
}
