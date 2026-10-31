//! bzip2's Huffman codes: lengths as libbzip2 makes them, canonical codes, and decoding.

use crate::{Result, corrupt};

/// Symbols of an alphabet, at most: 256 MTF values and EOB, with RUNA and RUNB for 0.
pub const MAX_ALPHA: usize = 258;
/// Bits of a code, at most: libbzip2 since 1.0.3 makes up to 17, decoders take up to 20.
pub const MAX_LEN: u32 = 20;
pub const MAKE_MAX_LEN: u32 = 17;

/// The code lengths libbzip2 (`BZ2_hbMakeCodeLengths`) gives for `freq`, of at most
/// `max_len` bits: a Huffman tree over the frequencies (0 counting as 1) built with a binary
/// heap, the two lightest merged first, a node's weight carrying its depth in its low 8 bits
/// so that among equal frequencies the shallower is lighter. Where a code is too long, every
/// frequency `f` becomes `1 + f / 2` and the tree is built again.
pub fn code_lengths(len: &mut [u8], freq: &[u32], max_len: u32) {
    let a = freq.len();
    assert!(a <= MAX_ALPHA && len.len() == a);
    let mut weight = [0u32; 2 * MAX_ALPHA];
    let mut parent = [0i32; 2 * MAX_ALPHA];
    let mut heap = [0usize; MAX_ALPHA + 2];
    for i in 0..a {
        weight[i + 1] = freq[i].max(1) << 8;
    }
    loop {
        let mut nodes = a;
        let mut n = 0;
        heap[0] = 0;
        weight[0] = 0;
        parent[0] = -2;
        // (heap[0], of weight 0, stops the sift up)
        let up = |heap: &mut [usize], weight: &[u32], mut z: usize| {
            let t = heap[z];
            while weight[t] < weight[heap[z >> 1]] {
                heap[z] = heap[z >> 1];
                z >>= 1;
            }
            heap[z] = t;
        };
        let down = |heap: &mut [usize], weight: &[u32], n: usize| {
            let mut z = 1;
            let t = heap[z];
            loop {
                let mut y = z << 1;
                if y > n {
                    break;
                }
                if y < n && weight[heap[y + 1]] < weight[heap[y]] {
                    y += 1;
                }
                if weight[t] < weight[heap[y]] {
                    break;
                }
                heap[z] = heap[y];
                z = y;
            }
            heap[z] = t;
        };
        parent[1..=a].fill(-1);
        for i in 1..=a {
            n += 1;
            heap[n] = i;
            up(&mut heap, &weight, n);
        }
        while n > 1 {
            let n1 = heap[1];
            heap[1] = heap[n];
            n -= 1;
            down(&mut heap, &weight, n);
            let n2 = heap[1];
            heap[1] = heap[n];
            n -= 1;
            down(&mut heap, &weight, n);
            nodes += 1;
            parent[n1] = nodes as i32;
            parent[n2] = nodes as i32;
            let (w1, w2) = (weight[n1], weight[n2]);
            weight[nodes] = ((w1 & !0xFF) + (w2 & !0xFF)) | (1 + (w1 & 0xFF).max(w2 & 0xFF));
            parent[nodes] = -1;
            n += 1;
            heap[n] = nodes;
            up(&mut heap, &weight, n);
        }
        let mut too_long = false;
        for i in 1..=a {
            let mut j = 0;
            let mut k = i;
            while parent[k] >= 0 {
                k = parent[k] as usize;
                j += 1;
            }
            len[i - 1] = j as u8;
            too_long |= j > max_len;
        }
        if !too_long {
            return;
        }
        for w in &mut weight[1..=a] {
            *w = (1 + (*w >> 8) / 2) << 8;
        }
    }
}

/// Canonical codes for `len`: by length, then by symbol.
pub fn codes(len: &[u8]) -> Vec<u32> {
    let mut code = vec![0u32; len.len()];
    let mut next = 0u32;
    for l in 1..=MAX_LEN as u8 {
        for (c, &x) in code.iter_mut().zip(len) {
            if x == l {
                *c = next;
                next += 1;
            }
        }
        next <<= 1;
    }
    code
}

/// Bits of a decoder's first-level table.
const FAST: u32 = 10;

/// A decoder of canonical codes, of at most [`MAX_LEN`] bits.
pub struct Decoder {
    /// By the next [`FAST`] bits: the symbol and its length, 0 for a longer code.
    fast: Vec<(u16, u8)>,
    /// For each length: the first code, how many there are, and where their symbols start in
    /// `sorted`.
    first: [u32; MAX_LEN as usize + 1],
    count: [u32; MAX_LEN as usize + 1],
    offset: [u32; MAX_LEN as usize + 1],
    sorted: Vec<u16>,
}

impl Decoder {
    /// Fails where the lengths, of 1 to [`MAX_LEN`] bits, are oversubscribed.
    pub fn new(len: &[u8]) -> Result<Decoder> {
        let mut count = [0u32; MAX_LEN as usize + 1];
        for &l in len {
            if !(1..=MAX_LEN as u8).contains(&l) {
                return Err(corrupt());
            }
            count[l as usize] += 1;
        }
        let (mut first, mut offset) = ([0u32; MAX_LEN as usize + 1], [0u32; MAX_LEN as usize + 1]);
        let (mut code, mut at) = (0u64, 0u32);
        for l in 1..=MAX_LEN as usize {
            first[l] = code as u32;
            offset[l] = at;
            code += count[l] as u64;
            at += count[l];
            if code > 1 << l {
                return Err(corrupt());
            }
            code <<= 1;
        }
        let mut sorted = Vec::with_capacity(len.len());
        for l in 1..=MAX_LEN as u8 {
            sorted.extend((0..len.len() as u16).filter(|&s| len[s as usize] == l));
        }
        let mut fast = vec![(0u16, 0u8); 1 << FAST];
        let code = codes(len);
        for (s, (&l, &c)) in len.iter().zip(&code).enumerate() {
            if l as u32 <= FAST {
                let lo = (c << (FAST - l as u32)) as usize;
                fast[lo..lo + (1 << (FAST - l as u32))].fill((s as u16, l));
            }
        }
        Ok(Decoder { fast, first, count, offset, sorted })
    }

    /// The symbol at the start of `w` (the next [`MAX_LEN`] bits, at the top of 32), and its
    /// length; None where no code is.
    #[inline(always)]
    pub fn decode(&self, w: u32) -> Option<(u16, u32)> {
        let (s, l) = self.fast[(w >> (32 - FAST)) as usize];
        if l != 0 {
            return Some((s, l as u32));
        }
        for l in FAST + 1..=MAX_LEN {
            let c = (w >> (32 - l)).wrapping_sub(self.first[l as usize]);
            if c < self.count[l as usize] {
                return Some((self.sorted[(self.offset[l as usize] + c) as usize], l));
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lengths_limited() {
        // Fibonacci frequencies make a tree as deep as the alphabet
        let mut freq = vec![1u32, 1];
        while freq.len() < 30 {
            freq.push(freq[freq.len() - 1] + freq[freq.len() - 2]);
        }
        let mut len = vec![0; freq.len()];
        code_lengths(&mut len, &freq, MAKE_MAX_LEN);
        assert!(len.iter().all(|&l| (1..=17).contains(&l)), "{len:?}");
        let kraft: f64 = len.iter().map(|&l| 0.5f64.powi(l as i32)).sum();
        assert_eq!(kraft, 1.0);
        let d = Decoder::new(&len).unwrap();
        for (s, &c) in codes(&len).iter().enumerate() {
            assert_eq!(d.decode(c << (32 - len[s] as u32)), Some((s as u16, len[s] as u32)));
        }
    }
}
