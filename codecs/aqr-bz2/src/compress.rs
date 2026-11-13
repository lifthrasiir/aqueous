//! libbzip2's compressor (as of 1.0.3 and on, at the default work factor), reimplemented:
//! where blocks are cut, then for each block RLE1, the BWT, MTF with runs of zeros as RUNA and
//! RUNB, and the Huffman tables and their selectors as libbzip2 picks them.

use aqr_common::msb::{BitReader, BitWriter, PAD};
use aqr_common::pool;

use crate::huffman::{self, MAKE_MAX_LEN, MAX_ALPHA};
use crate::stream::{GROUP, MAX_GROUPS};
use crate::{ALZ_BLOCK_MAGIC, ALZ_END_MAGIC, ALZ_LEVEL, BLOCK_MAGIC, END_MAGIC, Framing, MAGIC, block_max, crc, sais};

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
pub fn boundaries(data: &[u8], level: u8) -> Vec<usize> {
    let mut starts = Vec::new();
    each_block(data, level, &mut |a, _, _| {
        starts.push(a);
        Ok(())
    })
    .expect("nothing failing");
    starts
}

/// `f(start, end, syms)` for each block of `data` at `level`, in order: where it starts and
/// ends, and its symbols of RLE1 output. None for no data.
///
/// libbzip2 adds a byte to the block after checking that the block is not full (`nblockMAX`
/// symbols of RLE1 output). A run is added as it ends, so a block is full once a run's
/// symbols make it so, and the next byte, if there is one, ends it: the run that byte is part
/// of, under way, starts the next block.
pub fn each_block(
    data: &[u8],
    level: u8,
    f: &mut dyn FnMut(usize, usize, usize) -> crate::Result<()>,
) -> crate::Result<()> {
    let max = block_max(level);
    let (mut start, mut at, mut syms) = (0, 0, 0);
    let mut prev = None;
    for n in runs(data) {
        if let Some(p) = prev {
            // the run before ends at `at`, the start of this one
            syms += rle_syms(p);
            if syms >= max && at + 1 < data.len() {
                f(start, at, syms)?;
                (start, syms) = (at, 0);
            }
        }
        prev = Some(n);
        at += n;
    }
    match prev {
        Some(p) => f(start, at, syms + rle_syms(p)),
        None => Ok(()),
    }
}

/// Bytes of [`aqr_api::stack`] that compressing a block of `syms` symbols of RLE1 output takes
/// ([`write_block`]): the block written twice, its suffix array (the last symbols of the BWT
/// and the MTF symbols written over it after), [`sais::bwt_work`] (the selectors kept in it
/// after) and the suffixes' types.
pub fn block_plan(syms: usize) -> u64 {
    use aqr_api::stack::block;
    let m = syms as u64;
    block(2 * m) + block(8 * m) + block(4 * sais::bwt_work(syms) as u64) + block(2 * m)
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
    /// The bit `origPtr` starts at ([`orig_ptr_bit`]).
    orig_at: usize,
}

/// Where `origPtr` is in a block: after the magic, and the CRC and the randomised bit where
/// there are.
const fn orig_ptr_bit(framing: Framing) -> usize {
    match framing {
        Framing::Bzip2 => 48 + 32 + 1,
        Framing::Alz => 32,
    }
}

impl Block {
    /// The block with `p` for its `origPtr`, which only an order of equal rotations other than
    /// [`sais::bwt_in`]'s gives.
    pub fn set_orig_ptr(&mut self, p: u32) {
        self.orig_ptr = p;
        for k in 0..24 {
            let (i, b) = ((self.orig_at + k) / 8, 7 - (self.orig_at + k) % 8);
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

/// RLE1 of a block's data appended to `out`: each run of 4 to 255 equal bytes as 4 of them and
/// a count of more.
fn rle1(d: &[u8], out: &mut Vec<u8>) {
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
}

/// Compresses a block of data (none of it empty), cut where [`boundaries`] cuts.
pub fn block(d: &[u8]) -> Block {
    block_framed(d, Framing::Bzip2)
}

/// [`block`] in a stream of `framing`.
pub fn block_framed(d: &[u8], framing: Framing) -> Block {
    assert!(!d.is_empty());
    let mut w = BitWriter::with_capacity(d.len() / 2 + 1024);
    let (crc, orig_ptr, syms) = write_block(d, framing, None, &mut w).expect("no rank given");
    let nbits = w.len();
    let mut bits = w.finish();
    bits.resize(bits.len() + PAD, 0);
    Block { bits, nbits, crc, orig_ptr, syms, orig_at: orig_ptr_bit(framing) }
}

/// A block of data (none of it empty) compressed onto `w` ([`block_framed`]), of rank `rank`
/// where given in place of its own: its CRC, its rank and its symbols of RLE1 output. Fails only
/// for a rank given past the symbols. Takes [`block_plan`] of the symbols.
pub fn write_block(
    d: &[u8],
    framing: Framing,
    rank: Option<u32>,
    w: &mut BitWriter,
) -> crate::Result<(u32, u32, usize)> {
    let crc = crc::hash(d);
    let m = runs(d).map(rle_syms).sum::<usize>();
    let mut twice = Vec::with_capacity(2 * m);
    rle1(d, &mut twice);
    let (orig_ptr, m) = write_rle1(&mut twice, crc, framing, rank, w, &mut |_| Ok(()))?;
    Ok((crc, orig_ptr, m))
}

/// Called with the writer where a block has written enough to hand on the bytes it holds
/// (`w.out`, whole bytes), which it may take out.
pub type Spill<'a> = &'a mut dyn FnMut(&mut BitWriter) -> crate::Result<()>;

/// [`write_block`] of a block whose RLE1 output is `twice` (its CRC `crc`), with `spill` called as
/// the block is written (after its header, its tables and each group of its symbols), so that
/// what it holds stays small. `twice` is left as it was; its capacity should be that of its
/// length twice (it is written twice onto itself). Gives the rank and the symbols.
pub fn write_rle1(
    twice: &mut Vec<u8>,
    crc: u32,
    framing: Framing,
    rank: Option<u32>,
    w: &mut BitWriter,
    spill: Spill,
) -> crate::Result<(u32, usize)> {
    let m = twice.len();
    twice.extend_from_within(..);
    let mut used = [false; 256];
    twice[..m].iter().for_each(|&c| used[c as usize] = true);
    let mut sa = vec![0u32; 2 * m];
    let mut ws = vec![0u32; sais::bwt_work(m)];
    let mut st = vec![0u8; 2 * m];
    let own = sais::bwt_in(twice, &mut sa, &mut ws, &mut st);
    let orig_ptr = match rank {
        Some(p) if p as usize >= m => return Err(crate::corrupt()),
        Some(p) => p,
        None => own,
    };
    match framing {
        Framing::Bzip2 => {
            w.put((BLOCK_MAGIC >> 24) as u32, 24);
            w.put((BLOCK_MAGIC & 0xFF_FFFF) as u32, 24);
            w.put(crc, 32);
            w.put(0, 1);
        }
        Framing::Alz => w.put(ALZ_BLOCK_MAGIC, 32),
    }
    w.put(orig_ptr, 24);
    let (k, freq, n_used) = mtf(&mut sa, m, &used);
    twice.truncate(m);
    entropy(w, &sa[..k], &freq, n_used, &used, &mut ws, spill)?;
    Ok((orig_ptr, m))
}

/// MTF over the symbols in use, renumbered in byte order, with runs of the front symbol coded
/// as RUNA (0) and RUNB (1): a run of `r` is `r + 1` in binary, its leading 1 dropped, least
/// significant digit first, RUNA for a 0 and RUNB for a 1 (bijective base 2). MTF index `j`
/// is `j + 1`, then EOB. Of the `m` symbols at the start of `sa`, the MTF symbols written over
/// them (no more of them than were read, and EOB, so `sa` must have more than `m`): gives how
/// many, their frequencies and the symbols in use.
fn mtf(sa: &mut [u32], m: usize, used: &[bool; 256]) -> (usize, [u32; MAX_ALPHA], usize) {
    let mut seq = [0u8; 256];
    let mut n_used = 0;
    for c in 0..256 {
        if used[c] {
            seq[c] = n_used as u8;
            n_used += 1;
        }
    }
    let eob = n_used + 1;
    let mut freq = [0u32; MAX_ALPHA];
    let mut front = [0u8; 256];
    front.iter_mut().enumerate().for_each(|(i, f)| *f = i as u8);
    let front = &mut front[..n_used];
    let (mut run, mut k) = (0usize, 0);
    let flush = |run: &mut usize, sa: &mut [u32], k: &mut usize, freq: &mut [u32; MAX_ALPHA]| {
        if *run > 0 {
            let mut r = *run - 1;
            loop {
                let s = (r & 1) as u32;
                sa[*k] = s;
                *k += 1;
                freq[s as usize] += 1;
                if r < 2 {
                    break;
                }
                r = (r - 2) / 2;
            }
            *run = 0;
        }
    };
    for i in 0..m {
        let s = seq[sa[i] as usize];
        if front[0] == s {
            run += 1;
            continue;
        }
        flush(&mut run, sa, &mut k, &mut freq);
        let j = front.iter().position(|&x| x == s).unwrap();
        front.copy_within(0..j, 1);
        front[0] = s;
        sa[k] = j as u32 + 1;
        k += 1;
        freq[j + 1] += 1;
    }
    flush(&mut run, sa, &mut k, &mut freq);
    sa[k] = eob as u32;
    freq[eob] += 1;
    (k + 1, freq, n_used)
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
/// of the groups that took it. The selectors of the last pass are written; they are kept in
/// `sel` (a word for each group).
fn entropy(
    w: &mut BitWriter,
    mtf: &[u32],
    freq: &[u32],
    n_used: usize,
    used: &[bool; 256],
    sel: &mut [u32],
    spill: Spill,
) -> crate::Result<()> {
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

    let selectors = &mut sel[..n.div_ceil(GROUP)];
    let mut rfreq = [[0u32; MAX_ALPHA]; MAX_GROUPS];
    for _ in 0..PASSES {
        rfreq.iter_mut().for_each(|f| f.fill(0));
        for (g, sel) in mtf.chunks(GROUP).zip(selectors.iter_mut()) {
            let mut best = (u32::MAX, 0);
            for (t, l) in len[..groups].iter().enumerate() {
                let c: u32 = g.iter().map(|&s| l[s as usize] as u32).sum();
                if c < best.0 {
                    best = (c, t);
                }
            }
            let t = best.1;
            *sel = t as u32;
            g.iter().for_each(|&s| rfreq[t][s as usize] += 1);
        }
        for t in 0..groups {
            huffman::code_lengths(&mut len[t][..alpha], &rfreq[t][..alpha], MAKE_MAX_LEN);
        }
    }

    // the symbols in use: which of the 16 ranges of 16 bytes have any, then those ranges
    let mut any16 = [false; 16];
    for (a, r) in any16.iter_mut().zip(used.chunks(16)) {
        *a = r.iter().any(|&u| u);
    }
    any16.iter().for_each(|&a| w.put(a as u32, 1));
    for (r, _) in used.chunks(16).zip(&any16).filter(|(_, a)| **a) {
        r.iter().for_each(|&u| w.put(u as u32, 1));
    }
    spill(w)?;

    // the selectors, MTF coded in unary
    w.put(groups as u32, 3);
    w.put(selectors.len() as u32, 15);
    let mut front = [0, 1, 2, 3, 4, 5];
    for &s in selectors.iter() {
        let j = front.iter().position(|&x| x == s).unwrap();
        front.copy_within(0..j, 1);
        front[0] = s;
        w.put((1 << (j + 1)) - 2, j as u32 + 1);
    }
    spill(w)?;

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
        spill(w)?;
    }

    let mut codes = [[0u32; MAX_ALPHA]; MAX_GROUPS];
    for (c, l) in codes.iter_mut().zip(&len[..groups]) {
        huffman::codes_into(&l[..alpha], &mut c[..alpha]);
    }
    for (g, &t) in mtf.chunks(GROUP).zip(selectors.iter()) {
        let (l, c) = (&len[t as usize], &codes[t as usize]);
        g.iter().for_each(|&s| w.put(c[s as usize], l[s as usize] as u32));
        spill(w)?;
    }
    Ok(())
}

/// The blocks of `data` at `level` compressed, on up to `threads` threads.
pub fn blocks(data: &[u8], level: u8, threads: usize) -> Vec<Block> {
    blocks_framed(data, Framing::Bzip2, level, threads)
}

/// [`blocks`] in a stream of `framing` (whose level is [`ALZ_LEVEL`] for [`Framing::Alz`]).
pub fn blocks_framed(data: &[u8], framing: Framing, level: u8, threads: usize) -> Vec<Block> {
    debug_assert!(framing != Framing::Alz || level == ALZ_LEVEL);
    let mut cuts = boundaries(data, level);
    let n = cuts.len();
    cuts.push(data.len());
    pool::par_map(n, threads, |k| block_framed(&data[cuts[k]..cuts[k + 1]], framing))
}

/// The header of a stream at `level`: `BZh` and the level's digit, which [`Framing::Alz`] has not.
pub fn put_start(w: &mut BitWriter, framing: Framing, level: u8) {
    if framing == Framing::Bzip2 {
        MAGIC.iter().for_each(|&c| w.put(c as u32, 8));
        w.put((b'0' + level) as u32, 8);
    }
}

/// The end of a stream whose blocks' CRCs combine to `combined`.
pub fn put_end(w: &mut BitWriter, framing: Framing, combined: u32) {
    match framing {
        Framing::Bzip2 => {
            w.put((END_MAGIC >> 24) as u32, 24);
            w.put((END_MAGIC & 0xFF_FFFF) as u32, 24);
            w.put(combined, 32);
        }
        Framing::Alz => w.put(ALZ_END_MAGIC, 32),
    }
}

/// The stream of `blocks` at `level`: the header, the blocks, the end and the combined CRC.
pub fn stream(level: u8, blocks: &[Block]) -> Vec<u8> {
    stream_framed(Framing::Bzip2, level, blocks)
}

/// The stream of `blocks` (of [`Framing::Alz`]) as ALZip writes it: the blocks and the end.
pub fn stream_alz(blocks: &[Block]) -> Vec<u8> {
    stream_framed(Framing::Alz, ALZ_LEVEL, blocks)
}

/// The stream of `blocks` in `framing` at `level`: the header, the blocks and the end (as ALZip
/// writes them for [`Framing::Alz`]).
pub fn stream_framed(framing: Framing, level: u8, blocks: &[Block]) -> Vec<u8> {
    let bytes: usize = blocks.iter().map(|b| b.nbits / 8 + 1).sum();
    let mut w = BitWriter::with_capacity(bytes + 16);
    put_start(&mut w, framing, level);
    let mut combined = 0u32;
    for b in blocks {
        debug_assert!(framing != Framing::Alz || b.orig_at == orig_ptr_bit(Framing::Alz));
        w.copy(&mut BitReader::new(&b.bits, 0), b.nbits);
        combined = combined.rotate_left(1) ^ b.crc;
    }
    put_end(&mut w, framing, combined);
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
            let mut sa = vec![b'a' as u32; r];
            sa.extend([b'b' as u32, 0]);
            let (k, _, _) = mtf(&mut sa, r + 1, &used);
            assert_eq!(&sa[..want.len()], &want[..], "run {r}");
            assert_eq!(&sa[want.len()..k], &[2, 3]);
        }
    }
}
