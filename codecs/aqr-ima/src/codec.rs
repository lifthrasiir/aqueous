//! IMA ADPCM blocks as a record of their headers and nibbles, coded with a binary arithmetic coder
//! under adaptive bits in contexts of the decoder's state (FORMAT.md §4), and back.
//!
//! A block holds, per channel, a header (the predictor, 16 bits signed; the step index; a byte
//! reserved), then the channels' nibbles in words of 4 bytes, a word of each channel in turn,
//! each byte's low nibble first. Xbox ADPCM (FSB5's) has 36 bytes per channel; Microsoft's IMA
//! ADPCM (WAV) as many as its block alignment gives.

use aqr_common::arith::{ArithDecoder, ArithEncoder, Bit};

use crate::{Error, Result};

const STEPS: [i32; 89] = [
    7, 8, 9, 10, 11, 12, 13, 14, 16, 17, 19, 21, 23, 25, 28, 31, 34, 37, 41, 45, 50, 55, 60, 66, 73, 80, 88, 97, 107,
    118, 130, 143, 157, 173, 190, 209, 230, 253, 279, 307, 337, 371, 408, 449, 494, 544, 598, 658, 724, 796, 876, 963,
    1060, 1166, 1282, 1411, 1552, 1707, 1878, 2066, 2272, 2499, 2749, 3024, 3327, 3660, 4026, 4428, 4871, 5358, 5894,
    6484, 7132, 7845, 8630, 9493, 10442, 11487, 12635, 13899, 15289, 16818, 18500, 20350, 22385, 24623, 27086, 29794,
    32767,
];
const INDEX: [i32; 8] = [-1, -1, -1, -1, 2, 4, 6, 8];

/// The decoder's state: the predictor and the step index (0 to 88).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct State {
    pred: i32,
    idx: usize,
}

impl State {
    fn of(pred: i16, idx: u8) -> State {
        State { pred: pred as i32, idx: (idx as usize).min(88) }
    }

    /// The state after nibble `v` (the IMA decoder).
    #[inline]
    fn step(&mut self, v: u8) {
        let s = STEPS[self.idx];
        let mut d = s >> 3;
        if v & 4 != 0 {
            d += s;
        }
        if v & 2 != 0 {
            d += s >> 1;
        }
        if v & 1 != 0 {
            d += s >> 2;
        }
        self.pred = if v & 8 != 0 { self.pred - d } else { self.pred + d }.clamp(-32768, 32767);
        self.idx = (self.idx as i32 + INDEX[(v & 7) as usize]).clamp(0, 88) as usize;
    }
}

/// Codes decisions: the encoder codes `b` and returns it, the decoder returns what it decodes.
trait Coder {
    fn bit(&mut self, k: &mut Bit, b: bool) -> bool;
}

impl Coder for ArithEncoder {
    #[inline]
    fn bit(&mut self, k: &mut Bit, b: bool) -> bool {
        self.encode(b, k.p1());
        k.update(b);
        b
    }
}

impl Coder for ArithDecoder<'_> {
    #[inline]
    fn bit(&mut self, k: &mut Bit, _: bool) -> bool {
        let b = self.decode(k.p1());
        k.update(b);
        b
    }
}

/// Step-index buckets of the contexts (and one for a channel of no state yet).
const BUCKETS: usize = 16;
/// Nibble contexts: none before (16) or the nibble before.
const PREV: usize = 17;

fn bucket(s: Option<State>) -> usize {
    s.map_or(BUCKETS - 1, |s| (s.idx / 6).min(BUCKETS - 2))
}

/// The adaptive bits.
struct Model {
    /// Per channel past the first, whether it is as the one before, by that of the block before
    /// (0, 1, or 2 for the first block).
    same: Vec<[Bit; 3]>,
    reserved_zero: Bit,
    reserved: [Bit; 256],
    idx_next: [Bit; BUCKETS],
    idx: [Bit; 256],
    /// The predictor's difference: its zigzag value's bit count (unary), by bucket; its bits
    /// below the leading one, by count and position.
    pred_len: [[Bit; 18]; BUCKETS],
    pred_bits: [[Bit; 17]; 18],
    /// Nibbles: a tree of 4 bits, by bucket, the nibble before, and the one at the same place of
    /// the channel before (or none).
    nibble: Vec<Bit>,
}

impl Model {
    fn new(channels: usize) -> Model {
        Model {
            same: vec![[Bit::default(); 3]; channels],
            reserved_zero: Bit::default(),
            reserved: [Bit::default(); 256],
            idx_next: [Bit::default(); BUCKETS],
            idx: [Bit::default(); 256],
            pred_len: [[Bit::default(); 18]; BUCKETS],
            pred_bits: [[Bit::default(); 17]; 18],
            nibble: vec![Bit::default(); BUCKETS * PREV * PREV * 16],
        }
    }

    /// A value of `n` bits as a tree of counters `t` (of `2^n` of them), most significant first.
    fn tree(c: &mut dyn Coder, t: &mut [Bit], n: u32, v: u32) -> u32 {
        let mut node = 1usize;
        for i in (0..n).rev() {
            let b = c.bit(&mut t[node], v >> i & 1 == 1);
            node = node * 2 + b as usize;
        }
        (node - (1 << n)) as u32
    }

    /// A channel's header (`h`: its 4 bytes; any where decoding), against its state at the end of
    /// the block before.
    fn header(&mut self, c: &mut dyn Coder, end: Option<State>, h: [u8; 4]) -> [u8; 4] {
        let b = bucket(end);
        // the step index: the end's plus 1, or as it is
        let next = end.map_or(0, |s| s.idx as u32 + 1);
        let idx = if c.bit(&mut self.idx_next[b], h[2] as u32 == next && next < 256) {
            next as u8
        } else {
            Model::tree(c, &mut self.idx, 8, h[2] as u32) as u8
        };
        // the predictor, as a difference from the end's (zigzag)
        let base = end.map_or(0, |s| s.pred);
        let delta = i16::from_le_bytes([h[0], h[1]]) as i32 - base;
        let want = (delta << 1 ^ delta >> 31) as u32;
        let len = 32 - want.leading_zeros();
        let mut n = 0;
        while n < 17 && c.bit(&mut self.pred_len[b][n as usize], n < len) {
            n += 1;
        }
        let mut z = if n == 0 { 0 } else { 1u32 };
        for i in (0..n.saturating_sub(1)).rev() {
            z = z << 1 | c.bit(&mut self.pred_bits[n as usize][i as usize], want >> i & 1 == 1) as u32;
        }
        let pred = (base + ((z >> 1) as i32 ^ -((z & 1) as i32))) as i16;
        // the byte reserved: 0, or as it is
        let reserved = if c.bit(&mut self.reserved_zero, h[3] == 0) {
            0
        } else {
            Model::tree(c, &mut self.reserved, 8, h[3] as u32) as u8
        };
        let p = pred.to_le_bytes();
        [p[0], p[1], idx, reserved]
    }
}

/// The layout of blocks: channels, and bytes per channel (a header of 4, then words of 4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    pub channels: usize,
    pub per_channel: usize,
}

impl Layout {
    /// A layout of 1 to 255 channels and of 8 or more bytes per channel, a multiple of 4.
    pub fn new(channels: usize, per_channel: usize) -> Option<Layout> {
        ((1..=255).contains(&channels) && per_channel >= 8 && per_channel.is_multiple_of(4))
            .then_some(Layout { channels, per_channel })
    }

    /// Bytes per block.
    pub fn block(&self) -> usize {
        self.channels * self.per_channel
    }

    /// Nibbles per channel per block.
    fn nibbles(&self) -> usize {
        (self.per_channel - 4) * 2
    }
}

/// Codes the blocks of `d` (whole blocks of `l`) with `c`, writing them to `out` (decoding;
/// encoding gives `d` back).
fn code(cod: &mut dyn Coder, l: Layout, d: Option<&[u8]>, blocks: usize, out: &mut Vec<u8>) {
    let (ch, bs, n) = (l.channels, l.block(), l.nibbles());
    let mut m = Model::new(ch);
    let mut end: Vec<Option<State>> = vec![None; ch];
    let mut same_before = vec![2usize; ch];
    // each channel's nibbles of the block
    let mut nib = vec![vec![0u8; n]; ch];
    let mut block = vec![0u8; bs];
    for k in 0..blocks {
        let src = d.map(|d| &d[k * bs..(k + 1) * bs]);
        // a channel's nibble `j` at its place in the block
        let at = |c: usize, j: usize| 4 * ch + 4 * ((j / 8) * ch + c) + (j % 8) / 2;
        for c in 0..ch {
            if c > 0 {
                let want = src.is_some_and(|s| {
                    s[4 * c..4 * c + 4] == s[4 * (c - 1)..4 * c]
                        && (0..n / 8).all(|w| {
                            let (a, b) = (4 * ch + 4 * (w * ch + c), 4 * ch + 4 * (w * ch + c - 1));
                            s[a..a + 4] == s[b..b + 4]
                        })
                });
                let same = cod.bit(&mut m.same[c][same_before[c]], want);
                same_before[c] = same as usize;
                if same {
                    block.copy_within(4 * (c - 1)..4 * c, 4 * c);
                    for w in 0..n / 8 {
                        let (a, b) = (4 * ch + 4 * (w * ch + c), 4 * ch + 4 * (w * ch + c - 1));
                        block.copy_within(b..b + 4, a);
                    }
                    nib[c] = nib[c - 1].clone();
                    end[c] = end[c - 1];
                    continue;
                }
            }
            let h = src.map_or([0; 4], |s| s[4 * c..4 * c + 4].try_into().expect("4 bytes"));
            let h = m.header(cod, end[c], h);
            block[4 * c..4 * c + 4].copy_from_slice(&h);
            let mut s = State::of(i16::from_le_bytes([h[0], h[1]]), h[2]);
            let mut prev = PREV - 1;
            for j in 0..n {
                let want = src.map_or(0, |s| s[at(c, j)] >> (4 * (j % 2)) & 15);
                let lr = if c > 0 { nib[c - 1][j] as usize } else { PREV - 1 };
                let base = ((bucket(Some(s)) * PREV + prev) * PREV + lr) * 16;
                let v = Model::tree(cod, &mut m.nibble[base..base + 16], 4, want as u32) as u8;
                nib[c][j] = v;
                let p = at(c, j);
                block[p] = if j % 2 == 0 { (block[p] & 0xF0) | v } else { (block[p] & 0x0F) | v << 4 };
                s.step(v);
                prev = v as usize;
            }
            end[c] = Some(s);
        }
        out.extend_from_slice(&block);
    }
}

/// The record of the whole blocks of `l` at the start of `d`, and the bytes they take.
pub fn encode(l: Layout, d: &[u8]) -> (usize, Vec<u8>) {
    let blocks = d.len() / l.block();
    let mut c = ArithEncoder::new(Vec::new());
    let mut out = Vec::with_capacity(blocks * l.block());
    code(&mut c, l, Some(d), blocks, &mut out);
    debug_assert!(out == d[..blocks * l.block()]);
    (blocks * l.block(), c.finish())
}

/// The blocks of `record`, `len` bytes (a multiple of the block's).
pub fn decode(l: Layout, record: &[u8], len: usize) -> Result<Vec<u8>> {
    if !len.is_multiple_of(l.block()) {
        return Err(Error("not whole blocks"));
    }
    let mut c = ArithDecoder::new(record);
    let mut out = Vec::with_capacity(len);
    code(&mut c, l, None, len / l.block(), &mut out);
    if c.overrun() {
        return Err(Error("corrupt record"));
    }
    Ok(out)
}
