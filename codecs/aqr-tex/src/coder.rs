//! The two sides of the coder (FORMAT.md, "Coding"): the models run the same code to encode
//! and to decode ([`Side`]), the encoder given each value and the decoder taking it from the
//! coded bytes, so that they cannot differ. Values are coded as binary decisions of adaptive
//! bits ([`Bit`], aqr-common's carry-less binary arithmetic coder).

use aqr_common::arith::{ArithDecoder, ArithEncoder, Bit};

/// What a decision codes, for the encoder's tally of the bits each part takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cat {
    /// Where a block's endpoints come from: a neighbour, the cache, or new.
    Choice,
    /// The rank of endpoints in the cache.
    Cache,
    /// New endpoints, and BC1's colour mode.
    Endpoints,
    /// Indices.
    Indices,
    /// BC2's explicit alpha.
    Alpha,
}

pub const CATS: usize = 5;

pub trait Side {
    /// Whether the values given are coded (rather than taken from the coded bytes).
    const ENCODING: bool;

    /// Codes `v` (when encoding) by `b`, and gives the bit coded.
    fn bit(&mut self, b: &mut Bit, v: bool, cat: Cat) -> bool;

    /// `n` bits of `v` from the top, each of probability 1/2.
    fn raw(&mut self, n: u32, v: u32, cat: Cat) -> u32 {
        let mut x = 0;
        for i in (0..n).rev() {
            x = x << 1 | self.bit(&mut Bit::default(), v >> i & 1 != 0, cat) as u32;
        }
        x
    }

    /// `v < 2^n` by a binary tree of the adaptive bits `t` (at least `2^n` of them, the root at
    /// 1), from the top bit.
    #[inline]
    fn tree(&mut self, t: &mut [Bit], n: u32, v: u32, cat: Cat) -> u32 {
        let mut node = 1;
        for i in (0..n).rev() {
            node = node << 1 | self.bit(&mut t[node], v >> i & 1 != 0, cat) as usize;
        }
        node as u32 - (1 << n)
    }

    /// `v < 2^15` as its bit length by the tree `t` (16 bits), then the bit below its top one
    /// by `next[len]` (16 bits), then the rest raw.
    #[inline]
    fn token(&mut self, t: &mut [Bit], next: &mut [Bit], v: u32, cat: Cat) -> u32 {
        let len = self.tree(t, 4, u32::BITS - v.leading_zeros(), cat);
        match len {
            0 => 0,
            1 => 1,
            _ => {
                let second = self.bit(&mut next[len as usize], v >> (len - 2) & 1 != 0, cat) as u32;
                let rest = self.raw(len - 2, v, cat);
                (2 | second) << (len - 2) | rest
            }
        }
    }
}

pub struct Enc {
    pub c: ArithEncoder,
    /// Bits taken by each [`Cat`], in 1/2^16 bits.
    pub cost: [u64; CATS],
    log: &'static [u32; 4097],
}

/// −log2(i/4096) in 1/2^16 bits, for the tally of the encoder.
fn log_table() -> &'static [u32; 4097] {
    static T: std::sync::OnceLock<[u32; 4097]> = std::sync::OnceLock::new();
    T.get_or_init(|| std::array::from_fn(|i| (-(i.max(1) as f64 / 4096.0).log2() * 65536.0) as u32))
}

impl Enc {
    pub fn new() -> Enc {
        Enc { c: ArithEncoder::new(Vec::new()), cost: [0; CATS], log: log_table() }
    }
}

impl Default for Enc {
    fn default() -> Self {
        Enc::new()
    }
}

impl Side for Enc {
    const ENCODING: bool = true;

    #[inline]
    fn bit(&mut self, b: &mut Bit, v: bool, cat: Cat) -> bool {
        let p1 = b.p1();
        let p = if v { p1 } else { 65536 - p1 };
        self.cost[cat as usize] += self.log[(p >> 4) as usize] as u64;
        self.c.encode(v, p1);
        b.update(v);
        v
    }
}

pub struct Dec<'a> {
    pub c: ArithDecoder<'a>,
}

impl Side for Dec<'_> {
    const ENCODING: bool = false;

    #[inline]
    fn bit(&mut self, b: &mut Bit, _: bool, _: Cat) -> bool {
        let v = self.c.decode(b.p1());
        b.update(v);
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let vals: Vec<u32> = (0..2000u32).map(|i| i.wrapping_mul(2654435761) >> (17 + i % 15)).collect();
        let run = |s: &mut dyn FnMut(u32, u32) -> (u32, u32, u32)| -> Vec<(u32, u32, u32)> {
            vals.iter().enumerate().map(|(i, &v)| s(i as u32, v)).collect()
        };
        let mut e = Enc::new();
        let (mut t, mut tt, mut tn) = (vec![Bit::default(); 64], vec![Bit::default(); 16], vec![Bit::default(); 16]);
        let want = run(&mut |i, v| {
            (
                e.tree(&mut t, 6, v & 63, Cat::Indices),
                e.token(&mut tt, &mut tn, v, Cat::Cache),
                e.raw(3, i & 7, Cat::Alpha),
            )
        });
        let c = e.c.finish();
        let mut d = Dec { c: ArithDecoder::new(&c) };
        let (mut t, mut tt, mut tn) = (vec![Bit::default(); 64], vec![Bit::default(); 16], vec![Bit::default(); 16]);
        let got = run(&mut |_, _| {
            (d.tree(&mut t, 6, 0, Cat::Indices), d.token(&mut tt, &mut tn, 0, Cat::Cache), d.raw(3, 0, Cat::Alpha))
        });
        assert_eq!(got, want);
        assert!(want.iter().zip(&vals).all(|(w, &v)| w.0 == v & 63 && w.1 == v));
        assert!(!d.c.overrun());
    }
}
