//! LZFSE's entropy coding: tANS (FSE) tables of states given to the symbols in runs, the
//! backward bit streams they are read from, and the variable-length code of the frequencies in
//! a block header (`FORMAT.md` §3).

use crate::{Result, corrupt};

/// A table of `n` states (a power of 2), each symbol of frequency `f > 0` holding `f`
/// consecutive states, in the order of the symbols.
pub struct Table {
    /// Per state: the bits read, the symbol and the base of the next state.
    pub dec: Vec<(u8, u16, u32)>,
    /// Per symbol: its first state, frequency, `k` and `j0` (below).
    sym: Vec<(u32, u32, u32, u32)>,
    pub n: u32,
}

impl Table {
    /// The table of `freq`, whose sum must be at most `n` (states beyond it are none).
    pub fn new(freq: &[u16], n: u32) -> Result<Table> {
        let sum: u32 = freq.iter().map(|&f| f as u32).sum();
        if sum > n {
            return Err(corrupt());
        }
        let mut dec = Vec::with_capacity(n as usize);
        let mut sym = Vec::with_capacity(freq.len());
        for (s, &f) in freq.iter().enumerate() {
            let f = f as u32;
            if f == 0 {
                sym.push((0, 0, 0, 0));
                continue;
            }
            // f << k in [n, 2n); the first j0 states read k bits, the others k - 1
            let k = f.leading_zeros() - n.leading_zeros();
            let j0 = ((2 * n) >> k) - f;
            sym.push((dec.len() as u32, f, k, j0));
            for j in 0..f {
                dec.push(if j < j0 {
                    (k as u8, s as u16, ((f + j) << k) - n)
                } else {
                    ((k - 1) as u8, s as u16, (j - j0) << (k - 1))
                });
            }
        }
        Ok(Table { dec, sym, n })
    }

    /// The state before symbol `s` such that decoding it leads to `x`, and the bits to write
    /// (value, count). `s` must have a frequency.
    #[inline]
    pub fn encode(&self, s: usize, x: u32) -> (u32, u32, u32) {
        let (start, f, k, j0) = self.sym[s];
        debug_assert!(f > 0);
        if k > 0 && x < (f - j0) << (k - 1) {
            (start + j0 + (x >> (k - 1)), x & ((1 << (k - 1)) - 1), k - 1)
        } else {
            (start + ((x + self.n) >> k) - f, x & ((1 << k) - 1), k)
        }
    }

    pub fn has(&self, s: usize) -> bool {
        self.sym.get(s).is_some_and(|e| e.1 > 0)
    }
}

/// A bit stream read backward: from its top bit down, each field highest bit first. Bit `i` is
/// bit `i mod 8` of byte `i / 8`.
pub struct BackReader<'a> {
    d: &'a [u8],
    /// Bits left below.
    pub pos: usize,
}

impl<'a> BackReader<'a> {
    /// The stream of `d` but for its top `unused` bits.
    pub fn new(d: &'a [u8], unused: u32) -> Result<Self> {
        let pos = (d.len() * 8).checked_sub(unused as usize).ok_or_else(corrupt)?;
        Ok(BackReader { d, pos })
    }

    /// `n ≤ 32` bits.
    #[inline]
    pub fn read(&mut self, n: u32) -> Result<u32> {
        if n == 0 {
            return Ok(0);
        }
        let n = n as usize;
        if n > self.pos {
            return Err(corrupt());
        }
        self.pos -= n;
        let (byte, bit) = (self.pos / 8, self.pos % 8);
        let mut w = [0u8; 8];
        let end = (byte + 8).min(self.d.len());
        w[..end - byte].copy_from_slice(&self.d[byte..end]);
        Ok(((u64::from_le_bytes(w) >> bit) & ((1u64 << n) - 1)) as u32)
    }
}

/// A bit stream written forward, for [`BackReader`]: each field at the bits above those before.
#[derive(Default)]
pub struct Writer {
    pub out: Vec<u8>,
    acc: u64,
    bits: u32,
}

impl Writer {
    /// `n ≤ 32` bits of `v`.
    #[inline]
    pub fn put(&mut self, v: u32, n: u32) {
        self.acc |= (v as u64) << self.bits;
        self.bits += n;
        while self.bits >= 8 {
            self.out.push(self.acc as u8);
            self.acc >>= 8;
            self.bits -= 8;
        }
    }

    /// The bytes, and the unused bits at the top of the last (0 to 7).
    pub fn finish(mut self) -> (Vec<u8>, u32) {
        if self.bits == 0 {
            return (self.out, 0);
        }
        self.out.push(self.acc as u8);
        (self.out, 8 - self.bits)
    }
}

/// The code of a frequency `v` in a header: (bits, count). Values up to 1047.
pub fn freq_code(v: u32) -> (u32, u32) {
    match v {
        0 => (0, 2),
        1 => (2, 2),
        2 => (1, 3),
        3 => (5, 3),
        4..=7 => (3 | (v - 4) << 3, 5),
        8..=23 => (7 | (v - 8) << 4, 8),
        _ => (15 | (v - 24) << 4, 14),
    }
}

/// Decodes a frequency from the low bits of `x`: (value, bits).
pub fn freq_decode(x: u32) -> (u32, u32) {
    if x & 1 == 0 {
        (x >> 1 & 1, 2)
    } else if x & 3 == 1 {
        (2 + (x >> 2 & 1), 3)
    } else if x & 7 == 3 {
        (4 + (x >> 3 & 3), 5)
    } else if x & 15 == 7 {
        (8 + (x >> 4 & 15), 8)
    } else {
        (24 + (x >> 4 & 1023), 14)
    }
}

/// The frequencies of `n` states (a power of 2) for symbols counted `counts` (some nonzero), as
/// the compressor normalizes them (`FORMAT.md` §5.3).
pub fn normalize(counts: &[u32], n: u32, out: &mut [u16]) {
    let total: u64 = counts.iter().map(|&c| c as u64).sum();
    let mut left = n as i64;
    let (mut max, mut max_s) = (0, 0);
    for (s, &c) in counts.iter().enumerate() {
        let f = if c == 0 { 0 } else { ((2 * c as u64 * n as u64 + total) / (2 * total)).max(1) };
        out[s] = f as u16;
        left -= f as i64;
        if f > max {
            (max, max_s) = (f, s);
        }
    }
    if -left < (max >> 2) as i64 {
        out[max_s] = (out[max_s] as i64 + left) as u16;
        return;
    }
    // too many states given: taken back from the frequencies above 1, a share of each
    let mut over = -left;
    let mut shift = 3u32;
    while over > 0 {
        for f in out.iter_mut() {
            if *f > 1 {
                let k = (((*f - 1) >> shift) as i64).min(over);
                *f -= k as u16;
                over -= k;
                if over == 0 {
                    break;
                }
            }
        }
        shift = shift.saturating_sub(1);
    }
}
