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

/// zlib's `build_tree` + `gen_bitlen` (trees.c), bit-exact including tie breaking and the
/// overflow fixup. `freq` is indexed by symbol and has `elems` entries (286, 30 or 19).
/// Unused symbols get length 0; at least two codes are always produced.
pub fn zlib_lens(freq: &[u32], max_length: u8) -> Vec<u8> {
    const MAX_NODES: usize = 2 * 286 + 1;
    let elems = freq.len();
    assert!(elems <= 286);
    let nodes = 2 * elems + 1;
    let heap_size = nodes;
    // Fixed-size scratch on the stack: this runs for every block of every pass.
    let mut f = [0u32; MAX_NODES];
    f[..elems].copy_from_slice(freq);
    let mut depth = [0u8; MAX_NODES];
    let mut dad = [0usize; MAX_NODES];
    let mut len = [0u8; MAX_NODES];
    let mut heap = [0usize; MAX_NODES + 1];
    let mut heap_len = 0usize;
    let mut heap_max = heap_size;
    let mut max_code: isize = -1;

    for n in 0..elems {
        if f[n] != 0 {
            heap_len += 1;
            heap[heap_len] = n;
            max_code = n as isize;
            depth[n] = 0;
        }
    }
    while heap_len < 2 {
        let node = if max_code < 2 {
            max_code += 1;
            max_code as usize
        } else {
            0
        };
        heap_len += 1;
        heap[heap_len] = node;
        f[node] = 1;
        depth[node] = 0;
    }
    let max_code = max_code as usize;

    // zlib's `smaller`: by frequency, then by depth, i.e. by this key.
    let mut key = [0u64; MAX_NODES];
    for n in 0..elems {
        key[n] = (f[n] as u64) << 8 | depth[n] as u64;
    }
    let downheap = |heap: &mut [usize], heap_len: usize, key: &[u64], mut k: usize| {
        let v = heap[k];
        let mut j = k << 1;
        while j <= heap_len {
            if j < heap_len && key[heap[j + 1]] <= key[heap[j]] {
                j += 1;
            }
            if key[v] <= key[heap[j]] {
                break;
            }
            heap[k] = heap[j];
            k = j;
            j <<= 1;
        }
        heap[k] = v;
    };

    for k in (1..=heap_len / 2).rev() {
        downheap(&mut heap, heap_len, &key, k);
    }
    let mut node = elems;
    loop {
        let n = heap[1];
        heap[1] = heap[heap_len];
        heap_len -= 1;
        downheap(&mut heap, heap_len, &key, 1);
        let m = heap[1];
        heap_max -= 1;
        heap[heap_max] = n;
        heap_max -= 1;
        heap[heap_max] = m;
        f[node] = f[n] + f[m];
        depth[node] = depth[n].max(depth[m]) + 1;
        key[node] = (f[node] as u64) << 8 | depth[node] as u64;
        dad[n] = node;
        dad[m] = node;
        heap[1] = node;
        node += 1;
        downheap(&mut heap, heap_len, &key, 1);
        if heap_len < 2 {
            break;
        }
    }
    heap_max -= 1;
    heap[heap_max] = heap[1];

    // gen_bitlen
    let mut bl_count = [0u32; 16];
    let mut overflow = 0i32;
    len[heap[heap_max]] = 0;
    let mut h = heap_max + 1;
    while h < heap_size {
        let n = heap[h];
        let mut bits = len[dad[n]] + 1;
        if bits > max_length {
            bits = max_length;
            overflow += 1;
        }
        len[n] = bits;
        if n <= max_code {
            bl_count[bits as usize] += 1;
        }
        h += 1;
    }
    if overflow > 0 {
        loop {
            let mut bits = max_length as usize - 1;
            while bl_count[bits] == 0 {
                bits -= 1;
            }
            bl_count[bits] -= 1;
            bl_count[bits + 1] += 2;
            bl_count[max_length as usize] -= 1;
            overflow -= 2;
            if overflow <= 0 {
                break;
            }
        }
        h = heap_size;
        for bits in (1..=max_length as usize).rev() {
            let mut n = bl_count[bits];
            while n != 0 {
                h -= 1;
                let m = heap[h];
                if m > max_code {
                    continue;
                }
                len[m] = bits as u8;
                n -= 1;
            }
        }
    }
    let mut out = vec![0u8; elems];
    for n in 0..elems {
        // Symbols never pushed onto the heap keep length 0.
        if n <= max_code && f[n] != 0 {
            out[n] = len[n];
        }
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

/// zlib's `send_tree` over one tree (`lens` is `max_code + 1` long).
pub fn rle_zlib(lens: &[u8], out: &mut Vec<RleOp>) {
    let op = |sym: u8, extra: u8| RleOp { sym, extra };
    let mut prevlen: i32 = -1;
    let mut nextlen = lens.first().map_or(-1, |&l| l as i32);
    let mut count = 0usize;
    let (mut max_count, mut min_count) = if nextlen == 0 { (138, 3) } else { (7, 4) };
    for n in 0..lens.len() {
        let curlen = nextlen;
        nextlen = lens.get(n + 1).map_or(-1, |&l| l as i32);
        count += 1;
        if count < max_count && curlen == nextlen {
            continue;
        } else if count < min_count {
            for _ in 0..count {
                out.push(op(curlen as u8, 0));
            }
        } else if curlen != 0 {
            if curlen != prevlen {
                out.push(op(curlen as u8, 0));
                count -= 1;
            }
            out.push(op(16, (count - 3) as u8));
        } else if count <= 10 {
            out.push(op(17, (count - 3) as u8));
        } else {
            out.push(op(18, (count - 11) as u8));
        }
        count = 0;
        prevlen = curlen;
        (max_count, min_count) = if nextlen == 0 {
            (138, 3)
        } else if curlen == nextlen {
            (6, 3)
        } else {
            (7, 4)
        };
    }
}

/// libdeflate's `deflate_compute_precode_items` over the concatenated lengths.
pub fn rle_libdeflate(lens: &[u8], out: &mut Vec<RleOp>) {
    let n = lens.len();
    let mut run_start = 0;
    while run_start < n {
        let len = lens[run_start];
        let mut run_end = run_start + 1;
        while run_end < n && lens[run_end] == len {
            run_end += 1;
        }
        if len == 0 {
            while run_end - run_start >= 11 {
                let e = (run_end - run_start - 11).min(0x7f);
                out.push(RleOp {
                    sym: 18,
                    extra: e as u8,
                });
                run_start += 11 + e;
            }
            if run_end - run_start >= 3 {
                let e = (run_end - run_start - 3).min(7);
                out.push(RleOp {
                    sym: 17,
                    extra: e as u8,
                });
                run_start += 3 + e;
            }
        } else if run_end - run_start >= 4 {
            out.push(RleOp { sym: len, extra: 0 });
            run_start += 1;
            loop {
                let e = (run_end - run_start - 3).min(3);
                out.push(RleOp {
                    sym: 16,
                    extra: e as u8,
                });
                run_start += 3 + e;
                if run_end - run_start < 3 {
                    break;
                }
            }
        }
        while run_start != run_end {
            out.push(RleOp { sym: len, extra: 0 });
            run_start += 1;
        }
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
