//! Canonical prefix codes of up to 15 bits, read and written most significant bit first: codes
//! assigned in the order of (length, symbol), as RAR's streams have them.

use aqr_common::msb::{BitReader, BitWriter};

use crate::{Result, corrupt};

/// The longest code.
pub const MAX: usize = 15;
/// Bits looked up at once.
const QUICK: u32 = 10;

/// A decoder of a code given by its lengths.
pub struct Decoder {
    /// By the next [`QUICK`] bits: the symbol << 4 | its length, 0 where the code is longer or
    /// none.
    quick: Vec<u16>,
    /// By length: the end of its codes, left aligned to [`MAX`] bits; the first code; where its
    /// symbols start in `syms`.
    limit: [u32; MAX + 1],
    first: [u32; MAX + 1],
    offset: [u16; MAX + 1],
    syms: Vec<u16>,
}

/// The codes of `lens` (0 for no code): each symbol's code and length. Over-subscribed lengths
/// are an error; incomplete ones are not.
pub fn codes(lens: &[u8]) -> Result<Vec<(u16, u8)>> {
    let mut count = [0u32; MAX + 1];
    for &l in lens {
        if l as usize > MAX {
            return Err(corrupt());
        }
        count[l as usize] += 1;
    }
    count[0] = 0;
    let (mut next, mut code) = ([0u32; MAX + 1], 0u32);
    for l in 1..=MAX {
        code = (code + count[l - 1]) << 1;
        next[l] = code;
        if code + count[l] > 1 << l {
            return Err(corrupt());
        }
    }
    Ok(lens
        .iter()
        .map(|&l| {
            if l == 0 {
                return (0, 0);
            }
            let c = next[l as usize];
            next[l as usize] += 1;
            (c as u16, l)
        })
        .collect())
}

impl Decoder {
    pub fn new(lens: &[u8]) -> Result<Decoder> {
        let codes = codes(lens)?;
        let mut count = [0u32; MAX + 1];
        lens.iter().for_each(|&l| count[l as usize] += 1);
        count[0] = 0;
        let mut d = Decoder {
            quick: vec![0; 1 << QUICK],
            limit: [0; MAX + 1],
            first: [0; MAX + 1],
            offset: [0; MAX + 1],
            syms: vec![0; lens.len()],
        };
        let (mut code, mut at) = (0u32, 0u32);
        for l in 1..=MAX {
            code = (code + count[l - 1]) << 1;
            d.first[l] = code;
            d.offset[l] = at as u16;
            d.limit[l] = (code + count[l]) << (MAX - l);
            at += count[l];
        }
        let mut fill = d.offset;
        for (s, &l) in lens.iter().enumerate().filter(|e| *e.1 != 0) {
            d.syms[fill[l as usize] as usize] = s as u16;
            fill[l as usize] += 1;
        }
        for (s, &(c, l)) in codes.iter().enumerate() {
            if l == 0 || l as u32 > QUICK {
                continue;
            }
            let shift = QUICK - l as u32;
            let base = (c as usize) << shift;
            for e in &mut d.quick[base..base + (1 << shift)] {
                *e = (s as u16) << 4 | l as u16;
            }
        }
        Ok(d)
    }

    /// The next symbol of `r`; an error for a pattern that is no code.
    #[inline]
    pub fn read(&self, r: &mut BitReader) -> Result<u16> {
        let q = self.quick[r.peek(QUICK) as usize];
        if q != 0 {
            r.skip((q & 15) as u32);
            return Ok(q >> 4);
        }
        let v = r.peek(MAX as u32);
        for l in 1..=MAX {
            if v < self.limit[l] {
                let k = (v >> (MAX - l)) - self.first[l];
                r.skip(l as u32);
                return self.syms.get(self.offset[l] as usize + k as usize).copied().ok_or_else(corrupt);
            }
        }
        Err(corrupt())
    }
}

/// Writes symbol `s` of `codes`; an error where it has no code.
#[inline]
pub fn write(w: &mut BitWriter, codes: &[(u16, u8)], s: usize) -> Result<()> {
    let (c, l) = codes[s];
    if l == 0 {
        return Err(crate::Error("a symbol without a code"));
    }
    w.put(c as u32, l as u32);
    Ok(())
}
