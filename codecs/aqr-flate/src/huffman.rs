//! Huffman code construction: canonical codes for reading and writing, and the code-length
//! builders of known encoders that serve as predictions.

use crate::bits::BitReader;
use crate::{Error, Result};

pub const LEN_BASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
pub const LEN_EXTRA: [u8; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
pub const DIST_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
pub const DIST_EXTRA: [u8; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];
/// Transmission order of code-length code lengths.
pub const CL_ORDER: [usize; 19] = [
    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
];

/// Length code (0-based, i.e. symbol - 257) of a match length in 3..=258, canonical form.
pub fn len_code(len: u16) -> usize {
    debug_assert!((3..=258).contains(&len));
    LEN_BASE.partition_point(|&b| b <= len) - 1
}

pub fn dist_code(dist: u16) -> usize {
    debug_assert!(dist >= 1);
    DIST_BASE.partition_point(|&b| b <= dist) - 1
}

pub fn fixed_lit_lens() -> [u8; 288] {
    let mut l = [0u8; 288];
    l[..144].fill(8);
    l[144..256].fill(9);
    l[256..280].fill(7);
    l[280..].fill(8);
    l
}

pub const FIXED_DIST_LENS: [u8; 30] = [5; 30];

/// Canonical code words, bit-reversed for LSB-first emission.
pub fn codes(lens: &[u8]) -> Vec<u16> {
    let mut count = [0u16; 16];
    for &l in lens {
        count[l as usize] += 1;
    }
    count[0] = 0;
    let mut next = [0u16; 16];
    let mut code = 0u16;
    for b in 1..16 {
        code = (code + count[b - 1]) << 1;
        next[b] = code;
    }
    lens.iter()
        .map(|&l| {
            if l == 0 {
                return 0;
            }
            let c = next[l as usize];
            next[l as usize] += 1;
            c.reverse_bits() >> (16 - l)
        })
        .collect()
}

/// Kraft sum check: rejects oversubscribed codes. Incomplete codes are allowed (whether a
/// decoder accepts them is up to the stream having parsed in the first place).
pub fn check_lens(lens: &[u8]) -> Result<()> {
    let mut kraft = 0u32;
    for &l in lens {
        if l > 0 {
            kraft += 1 << (15 - l);
        }
    }
    if kraft > 1 << 15 {
        return Err(Error::Invalid("oversubscribed Huffman code"));
    }
    Ok(())
}

/// Table-driven decoder for one canonical code.
pub struct Decoder {
    /// Indexed by the next `max` bits: (symbol, length), length 0 = invalid.
    table: Vec<(u16, u8)>,
    max: u32,
}

impl Decoder {
    pub fn new(lens: &[u8]) -> Result<Self> {
        check_lens(lens)?;
        let max = lens.iter().copied().max().unwrap_or(0).max(1) as u32;
        let mut table = vec![(0u16, 0u8); 1 << max];
        for (sym, (&l, &c)) in lens.iter().zip(&codes(lens)).enumerate() {
            if l == 0 {
                continue;
            }
            let mut i = c as usize;
            while i < table.len() {
                table[i] = (sym as u16, l);
                i += 1 << l;
            }
        }
        Ok(Decoder { table, max })
    }

    pub fn decode(&self, r: &mut BitReader) -> Result<u16> {
        let (sym, l) = self.table[r.peek(self.max) as usize];
        if l == 0 {
            return Err(Error::Invalid("undecodable Huffman code"));
        }
        r.consume(l as u32)?;
        Ok(sym)
    }
}

/// zlib's code lengths `zlib(freq, max_length)` (FORMAT.md, "Code lengths"); `freq` has at
/// most 286 entries.
pub fn zlib_lens(freq: &[u32], max_length: u8) -> Vec<u8> {
    let n = freq.len();
    let max_len = max_length as usize;
    // A node is (frequency, depth, id); the heap's weight is the first two, and the layout
    // alone settles ties. Leaves have their symbol as id, forced ones included.
    type Node = (u64, u32, usize);
    let mut nodes: Vec<Node> = vec![];
    let mut top: isize = -1;
    for (i, &f) in freq.iter().enumerate() {
        if f != 0 {
            nodes.push((f as u64, 0, i));
            top = i as isize;
        }
    }
    while nodes.len() < 2 {
        let sym = if top < 2 { (top + 1) as usize } else { 0 };
        if top < 2 {
            top += 1;
        }
        nodes.push((1, 0, sym));
    }
    let base = n.max(top as usize + 1);
    let no_heavier = |a: &Node, b: &Node| (a.0, a.1) <= (b.0, b.1);
    let sink = |heap: &mut Vec<Node>, mut k: usize| {
        // `heap[0]` is unused, so that the children of `k` are at `2k` and `2k + 1`.
        let node = heap[k];
        let m = heap.len() - 1;
        while 2 * k <= m {
            let mut c = 2 * k;
            if c < m && no_heavier(&heap[c + 1], &heap[c]) {
                c += 1;
            }
            if no_heavier(&node, &heap[c]) {
                break;
            }
            heap[k] = heap[c];
            k = c;
        }
        heap[k] = node;
    };
    let mut heap = vec![(0, 0, 0)];
    heap.extend(nodes);
    for k in (1..=(heap.len() - 1) / 2).rev() {
        sink(&mut heap, k);
    }
    let mut parent = vec![0usize; base + heap.len()];
    let mut list: Vec<usize> = vec![];
    let mut next = base;
    while heap.len() > 2 {
        let x = heap[1];
        heap[1] = heap.pop().unwrap();
        sink(&mut heap, 1);
        let y = heap[1];
        list.extend([x.2, y.2]);
        parent[x.2] = next;
        parent[y.2] = next;
        heap[1] = (x.0 + y.0, x.1.max(y.1) + 1, next);
        sink(&mut heap, 1);
        next += 1;
    }
    list.push(heap[1].2);

    let mut len = vec![0usize; next];
    let mut count = vec![0usize; max_len + 1];
    let mut overflow = 0i64;
    for &node in list[..list.len() - 1].iter().rev() {
        let mut l = len[parent[node]] + 1;
        if l > max_len {
            l = max_len;
            overflow += 1;
        }
        len[node] = l;
        if node as isize <= top {
            count[l] += 1;
        }
    }
    if overflow > 0 {
        while overflow > 0 {
            let mut l = max_len - 1;
            while count[l] == 0 {
                l -= 1;
            }
            count[l] -= 1;
            count[l + 1] += 2;
            count[max_len] -= 1;
            overflow -= 2;
        }
        let mut bits = max_len;
        let mut left = count[bits];
        for &node in list.iter().filter(|&&node| node as isize <= top) {
            while left == 0 {
                bits -= 1;
                left = count[bits];
            }
            len[node] = bits;
            left -= 1;
        }
    }
    let mut out = vec![0u8; n];
    for &node in list
        .iter()
        .filter(|&&node| node as isize <= top && node < n)
    {
        out[node] = len[node] as u8;
    }
    out
}

/// One instruction of the code-length encoding: a symbol 0..=18 and its extra bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RleOp {
    pub sym: u8,
    pub extra: u8,
}

impl RleOp {
    pub fn extra_bits(sym: u8) -> u32 {
        match sym {
            16 => 2,
            17 => 3,
            18 => 7,
            _ => 0,
        }
    }

    /// Number of code lengths this op covers.
    pub fn count(self) -> usize {
        match self.sym {
            16 => 3 + self.extra as usize,
            17 => 3 + self.extra as usize,
            18 => 11 + self.extra as usize,
            _ => 1,
        }
    }
}

/// zlib's run-length coding of one tree's lengths (FORMAT.md, "Run-length codings"),
/// appended to `out`.
pub fn rle_zlib(lens: &[u8], out: &mut Vec<RleOp>) {
    let op = |sym: u8, extra: usize| RleOp {
        sym,
        extra: extra as u8,
    };
    let mut i = 0;
    let mut previous = None;
    let mut limits = match lens.first() {
        Some(0) => (138, 3),
        _ => (7, 4),
    };
    while i < lens.len() {
        let v = lens[i];
        let k = lens[i..]
            .iter()
            .take(limits.0)
            .take_while(|&&l| l == v)
            .count();
        i += k;
        if k < limits.1 {
            out.extend(std::iter::repeat_n(op(v, 0), k));
        } else if v != 0 {
            if previous != Some(v) {
                out.push(op(v, 0));
                out.push(op(16, k - 4));
            } else {
                out.push(op(16, k - 3));
            }
        } else if k <= 10 {
            out.push(op(17, k - 3));
        } else {
            out.push(op(18, k - 11));
        }
        previous = Some(v);
        limits = match lens.get(i) {
            Some(0) => (138, 3),
            Some(&u) if u == v => (6, 3),
            _ => (7, 4),
        };
    }
}

/// libdeflate's run-length coding of the concatenated lengths (FORMAT.md, "Run-length
/// codings"), appended to `out`.
pub fn rle_libdeflate(lens: &[u8], out: &mut Vec<RleOp>) {
    let op = |sym: u8, extra: usize| RleOp {
        sym,
        extra: extra as u8,
    };
    for run in lens.chunk_by(|a, b| a == b) {
        let v = run[0];
        let mut rest = run.len();
        if v == 0 {
            while rest >= 11 {
                let x = (rest - 11).min(127);
                out.push(op(18, x));
                rest -= x + 11;
            }
            if rest >= 3 {
                let x = (rest - 3).min(7);
                out.push(op(17, x));
                rest -= x + 3;
            }
        } else if rest >= 4 {
            out.push(op(v, 0));
            rest -= 1;
            while rest >= 3 {
                let x = (rest - 3).min(3);
                out.push(op(16, x));
                rest -= x + 3;
            }
        }
        out.extend(std::iter::repeat_n(op(v, 0), rest));
    }
}

/// Expands code-length ops back to lengths. Fails on a leading repeat or overrun.
pub fn rle_expand(ops: &[RleOp], total: usize) -> Result<Vec<u8>> {
    let mut lens = Vec::with_capacity(total);
    for op in ops {
        match op.sym {
            0..=15 => lens.push(op.sym),
            16 => {
                let &prev = lens
                    .last()
                    .ok_or(Error::Invalid("repeat with no previous length"))?;
                lens.extend(std::iter::repeat_n(prev, op.count()));
            }
            _ => lens.extend(std::iter::repeat_n(0, op.count())),
        }
        if lens.len() > total {
            return Err(Error::Invalid("code lengths overrun"));
        }
    }
    if lens.len() != total {
        return Err(Error::Invalid("code lengths underrun"));
    }
    Ok(lens)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn len_and_dist_codes() {
        assert_eq!(len_code(3), 0);
        assert_eq!(len_code(10), 7);
        assert_eq!(len_code(11), 8);
        assert_eq!(len_code(257), 27);
        assert_eq!(len_code(258), 28);
        assert_eq!(dist_code(1), 0);
        assert_eq!(dist_code(4), 3);
        assert_eq!(dist_code(5), 4);
        assert_eq!(dist_code(32768), 29);
    }

    #[test]
    fn zlib_forces_two_codes() {
        assert_eq!(zlib_lens(&[0; 30], 15)[..3], [1, 1, 0]);
        let mut f = [0u32; 30];
        f[5] = 7;
        assert_eq!(zlib_lens(&f, 15)[..6], [1, 0, 0, 0, 0, 1]);
    }

    #[test]
    fn rle_round_trip() {
        let lens: Vec<u8> = [8, 8, 8, 8, 8, 8, 8, 8, 0, 0, 0, 0, 5, 0, 7, 7, 7]
            .into_iter()
            .chain(std::iter::repeat_n(0, 150))
            .chain([3, 3, 3])
            .collect();
        for f in [rle_zlib, rle_libdeflate] {
            let mut ops = vec![];
            f(&lens, &mut ops);
            assert_eq!(rle_expand(&ops, lens.len()).unwrap(), lens);
        }
    }
}
