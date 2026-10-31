//! libbzip2's compressor (as of 1.0.3 and on, at the default work factor), reimplemented:
//! where blocks are cut, then for each block RLE1, the BWT, MTF with runs of zeros as RUNA and
//! RUNB, and the Huffman tables and their selectors as libbzip2 picks them.

use aqr_common::msb::{BitReader, BitWriter, PAD};
use aqr_common::pool;

use crate::huffman::{self, MAKE_MAX_LEN, MAX_ALPHA};
use crate::stream::{GROUP, MAX_GROUPS};
use crate::{BLOCK_MAGIC, END_MAGIC, MAGIC, block_max, crc, sais};

/// Symbols of RLE1 output for a run of `n` (1..=255) equal bytes.
#[inline(always)]
fn rle_syms(n: usize) -> usize {
    if n < 4 { n } else { 5 }
}

/// Lengths of the runs of RLE1 in `d`: of equal bytes, 255 at most, from the start.
fn runs(d: &[u8]) -> impl Iterator<Item = usize> + '_ {
    let mut i = 0;
    std::iter::from_fn(move || {
        if i >= d.len() {
            return None;
        }
        let c = d[i];
        let n = d[i..d.len().min(i + 255)].iter().take_while(|&&b| b == c).count();
        i += n;
        Some(n)
    })
}

/// Where the blocks of `data` start, at `level`: none for no data, else 0 and the others.
///
/// libbzip2 adds a byte to the block after checking that the block is not full (`nblockMAX`
/// symbols of RLE1 output). A run is added as it ends, so a block is full once a run's
/// symbols make it so, and the next byte, if there is one, ends it: the run that byte is part
/// of, under way, starts the next block.
pub fn boundaries(data: &[u8], level: u8) -> Vec<usize> {
    let max = block_max(level);
    let mut starts = Vec::new();
    if data.is_empty() {
        return starts;
    }
    starts.push(0);
    let (mut at, mut syms) = (0, 0);
    let mut prev = None;
    for n in runs(data) {
        if let Some(p) = prev {
            // the run before ends at `at`, the start of this one
            syms += rle_syms(p);
            if syms >= max && at + 1 < data.len() {
                starts.push(at);
                syms = 0;
            }
        }
        prev = Some(n);
        at += n;
    }
    starts
}

/// A block compressed: from its magic to its last symbol, MSB first.
#[derive(Debug, Clone)]
pub struct Block {
    /// The bits, followed by [`PAD`] zero bytes.
    pub bits: Vec<u8>,
    pub nbits: usize,
    pub crc: u32,
    pub orig_ptr: u32,
    /// Symbols of RLE1 output, rotated by the BWT.
    pub syms: usize,
}

/// Where `origPtr` is in a block: after the magic, the CRC and the randomised bit.
const ORIG_PTR_BIT: usize = 48 + 32 + 1;

impl Block {
    /// The block with `p` for its `origPtr`, which only an order of equal rotations other than
    /// [`sais::bwt`]'s gives.
    pub fn set_orig_ptr(&mut self, p: u32) {
        self.orig_ptr = p;
        for k in 0..24 {
            let (i, b) = ((ORIG_PTR_BIT + k) / 8, 7 - (ORIG_PTR_BIT + k) % 8);
            let v = (p >> (23 - k) & 1) as u8;
            self.bits[i] = self.bits[i] & !(1 << b) | v << b;
        }
    }

    /// Whether the block's bits are those of `d` from bit `at`.
    pub fn matches(&self, d: &[u8], at: usize) -> bool {
        if (at + self.nbits).div_ceil(8) > d.len() {
            return false;
        }
        let padded;
        let d = if (at + self.nbits) / 8 + PAD <= d.len() {
            d
        } else {
            padded = aqr_common::msb::padded(d);
            &padded
        };
        let (mut a, mut b) = (BitReader::new(&self.bits, 0), BitReader::new(d, at));
        let mut n = self.nbits;
        while n > 0 {
            let k = n.min(32) as u32;
            if a.get(k) != b.get(k) {
                return false;
            }
            n -= k as usize;
        }
        true
    }
}

/// RLE1 of a block's data: each run of 4 to 255 equal bytes as 4 of them and a count of more.
fn rle1(d: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(d.len() + d.len() / 4 + 5);
    let mut i = 0;
    for n in runs(d) {
        let c = d[i];
        i += n;
        if n < 4 {
            out.extend(std::iter::repeat_n(c, n));
        } else {
            out.extend_from_slice(&[c, c, c, c, (n - 4) as u8]);
        }
    }
    out
}

/// Compresses a block of data (none of it empty), cut where [`boundaries`] cuts.
pub fn block(d: &[u8]) -> Block {
    assert!(!d.is_empty());
    let crc = crc::hash(d);
    let rle = rle1(d);
    let mut used = [false; 256];
    rle.iter().for_each(|&c| used[c as usize] = true);
    let (last, orig_ptr) = sais::bwt(&rle);
    let syms = last.len();
    drop(rle);
    let mut w = BitWriter::with_capacity(last.len() / 2 + 1024);
    w.put((BLOCK_MAGIC >> 24) as u32, 24);
    w.put((BLOCK_MAGIC & 0xFF_FFFF) as u32, 24);
    w.put(crc, 32);
    w.put(0, 1);
    w.put(orig_ptr, 24);
    let (mtf, freq, n_used) = mtf(&last, &used);
    drop(last);
    entropy(&mut w, &mtf, &freq, n_used, &used);
    let nbits = w.len();
    let mut bits = w.finish();
    bits.resize(bits.len() + PAD, 0);
    Block { bits, nbits, crc, orig_ptr, syms }
}

/// MTF over the symbols in use, renumbered in byte order, with runs of the front symbol coded
/// as RUNA (0) and RUNB (1): a run of `r` is `r + 1` in binary, its leading 1 dropped, least
/// significant digit first, RUNA for a 0 and RUNB for a 1 (bijective base 2). MTF index `j`
/// is `j + 1`, then EOB. Gives the symbols, their frequencies and the symbols in use.
fn mtf(last: &[u8], used: &[bool; 256]) -> (Vec<u16>, Vec<u32>, usize) {
    let mut seq = [0u8; 256];
    let mut n_used = 0;
    for c in 0..256 {
        if used[c] {
            seq[c] = n_used as u8;
            n_used += 1;
        }
    }
    let eob = n_used + 1;
    let mut freq = vec![0u32; eob + 1];
    let mut out = Vec::with_capacity(last.len() + 1);
    let mut front: Vec<u8> = (0..n_used).map(|i| i as u8).collect();
    let mut run = 0usize;
    let flush = |run: &mut usize, out: &mut Vec<u16>, freq: &mut Vec<u32>| {
        if *run > 0 {
            let mut r = *run - 1;
            loop {
                let s = (r & 1) as u16;
                out.push(s);
                freq[s as usize] += 1;
                if r < 2 {
                    break;
                }
                r = (r - 2) / 2;
            }
            *run = 0;
        }
    };
    for &c in last {
        let s = seq[c as usize];
        if front[0] == s {
            run += 1;
            continue;
        }
        flush(&mut run, &mut out, &mut freq);
        let j = front.iter().position(|&x| x == s).unwrap();
        front.copy_within(0..j, 1);
        front[0] = s;
        out.push(j as u16 + 1);
        freq[j + 1] += 1;
    }
    flush(&mut run, &mut out, &mut freq);
    out.push(eob as u16);
    freq[eob] += 1;
    (out, freq, n_used)
}

/// Code lengths of the tables at first: higher costs for symbols outside a table's range.
const LESSER: u8 = 0;
const GREATER: u8 = 15;
/// Passes refining the tables.
const PASSES: usize = 4;

/// The rest of a block after `origPtr`: the symbols in use, the tables, the selectors and
/// the symbols coded, as libbzip2's `sendMTFValues` chooses them.
///
/// The number of tables goes by the number of symbols. At first each table covers a range of
/// symbol values (cost 0 inside, 15 outside), the ranges cut so that each holds about an equal
/// share of what is left of the frequencies, from the last table down; a range is one value
/// shorter where it ends past the share, for every other table (2nd, 4th, ... cut), neither the
/// first nor the last cut. Then four passes: each group of 50 symbols takes the table that codes
/// it cheapest (the first among equals), each table's lengths are rebuilt from the frequencies
/// of the groups that took it. The selectors of the last pass are written.
fn entropy(w: &mut BitWriter, mtf: &[u16], freq: &[u32], n_used: usize, used: &[bool; 256]) {
    let alpha = n_used + 2;
    let n = mtf.len();
    let groups = match n {
        0..200 => 2,
        200..600 => 3,
        600..1200 => 4,
        1200..2400 => 5,
        _ => 6,
    };
    let mut len = [[GREATER; MAX_ALPHA]; MAX_GROUPS];
    let (mut left, mut gs) = (n as u32, 0usize);
    for part in (1..=groups).rev() {
        let share = left / part as u32;
        let (mut ge, mut got) = (gs as isize - 1, 0u32);
        while got < share && ge < alpha as isize - 1 {
            ge += 1;
            got += freq[ge as usize];
        }
        if ge > gs as isize && part != groups && part != 1 && (groups - part) % 2 == 1 {
            got -= freq[ge as usize];
            ge -= 1;
        }
        for (v, l) in len[part - 1][..alpha].iter_mut().enumerate() {
            *l = if v >= gs && v as isize <= ge { LESSER } else { GREATER };
        }
        gs = (ge + 1) as usize;
        left -= got;
    }

    let mut selectors = Vec::with_capacity(n.div_ceil(GROUP));
    let mut rfreq = [[0u32; MAX_ALPHA]; MAX_GROUPS];
    for _ in 0..PASSES {
        selectors.clear();
        rfreq.iter_mut().for_each(|f| f.fill(0));
        for g in mtf.chunks(GROUP) {
            let mut best = (u32::MAX, 0);
            for (t, l) in len[..groups].iter().enumerate() {
                let c: u32 = g.iter().map(|&s| l[s as usize] as u32).sum();
                if c < best.0 {
                    best = (c, t);
                }
            }
            let t = best.1;
            selectors.push(t as u8);
            g.iter().for_each(|&s| rfreq[t][s as usize] += 1);
        }
        for t in 0..groups {
            huffman::code_lengths(&mut len[t][..alpha], &rfreq[t][..alpha], MAKE_MAX_LEN);
        }
    }

    // the symbols in use: which of the 16 ranges of 16 bytes have any, then those ranges
    let any16: Vec<bool> = used.chunks(16).map(|r| r.iter().any(|&u| u)).collect();
    any16.iter().for_each(|&a| w.put(a as u32, 1));
    for (r, _) in used.chunks(16).zip(&any16).filter(|(_, a)| **a) {
        r.iter().for_each(|&u| w.put(u as u32, 1));
    }

    // the selectors, MTF coded in unary
    w.put(groups as u32, 3);
    w.put(selectors.len() as u32, 15);
    let mut front: Vec<u8> = (0..groups as u8).collect();
    for &s in &selectors {
        let j = front.iter().position(|&x| x == s).unwrap();
        front.copy_within(0..j, 1);
        front[0] = s;
        w.put((1 << (j + 1)) - 2, j as u32 + 1);
    }

    // the tables' lengths, each from the one before: 10 up, 11 down, 0 to the next symbol
    for l in &len[..groups] {
        let mut cur = l[0];
        w.put(cur as u32, 5);
        for &x in &l[..alpha] {
            while cur < x {
                w.put(2, 2);
                cur += 1;
            }
            while cur > x {
                w.put(3, 2);
                cur -= 1;
            }
            w.put(0, 1);
        }
    }

    let codes: Vec<Vec<u32>> = len[..groups].iter().map(|l| huffman::codes(&l[..alpha])).collect();
    for (g, &t) in mtf.chunks(GROUP).zip(&selectors) {
        let (l, c) = (&len[t as usize], &codes[t as usize]);
        g.iter().for_each(|&s| w.put(c[s as usize], l[s as usize] as u32));
    }
}

/// The blocks of `data` at `level` compressed, on up to `threads` threads.
pub fn blocks(data: &[u8], level: u8, threads: usize) -> Vec<Block> {
    let mut cuts = boundaries(data, level);
    let n = cuts.len();
    cuts.push(data.len());
    pool::par_map(n, threads, |k| block(&data[cuts[k]..cuts[k + 1]]))
}

/// The stream of `blocks` at `level`: the header, the blocks, the end and the combined CRC.
pub fn stream(level: u8, blocks: &[Block]) -> Vec<u8> {
    let bytes: usize = blocks.iter().map(|b| b.nbits / 8 + 1).sum();
    let mut w = BitWriter::with_capacity(bytes + 16);
    MAGIC.iter().for_each(|&c| w.put(c as u32, 8));
    w.put((b'0' + level) as u32, 8);
    let mut combined = 0u32;
    for b in blocks {
        w.copy(&mut BitReader::new(&b.bits, 0), b.nbits);
        combined = combined.rotate_left(1) ^ b.crc;
    }
    w.put((END_MAGIC >> 24) as u32, 24);
    w.put((END_MAGIC & 0xFF_FFFF) as u32, 24);
    w.put(combined, 32);
    w.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cuts_carry_the_run_under_way() {
        // level 1: full at 99981 symbols
        let max = block_max(1);
        // distinct-ish bytes, one symbol each: the block fills at byte `max`, the next starts there
        let d: Vec<u8> = (0..max + 10).map(|i| (i % 2) as u8).collect();
        assert_eq!(boundaries(&d, 1), [0, max]);
        // exactly full, nothing after the last run: one block
        assert_eq!(boundaries(&d[..max + 1], 1), [0]);
        assert_eq!(boundaries(&d[..max + 2], 1), [0, max]);
        // runs of 255 (5 symbols): full after 19997 runs, the 19998th under way starts the next
        let r: Vec<u8> = (0..20000 * 255).map(|i| (i / 255 % 2) as u8).collect();
        assert_eq!(boundaries(&r, 1), [0, 19997 * 255]);
        assert!(boundaries(&[], 9).is_empty());
    }

    #[test]
    fn runs_coded_bijectively() {
        let used = {
            let mut u = [false; 256];
            u[b'a' as usize] = true;
            u[b'b' as usize] = true;
            u
        };
        for (r, want) in
            [(1, vec![0]), (2, vec![1]), (3, vec![0, 0]), (4, vec![1, 0]), (5, vec![0, 1]), (6, vec![1, 1])]
        {
            let mut last = vec![b'a'; r];
            last.push(b'b');
            let (m, _, _) = mtf(&last, &used);
            assert_eq!(&m[..want.len()], &want[..], "run {r}");
            assert_eq!(&m[want.len()..], &[2, 3]);
        }
    }
}
