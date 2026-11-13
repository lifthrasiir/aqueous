//! Microsoft ADPCM blocks as a record of their headers and nibbles, coded with a binary
//! arithmetic coder under adaptive bits in contexts of the decoder's state (FORMAT.md §4), and
//! back.
//!
//! A block of `C` channels holds a header: each channel's predictor (its coefficients' index,
//! u8), then each channel's delta, then its sample 1, then its sample 2 (i16 each, little
//! endian); then the nibbles, high nibble first, the channels' in turn (a nibble of each channel
//! a sample). The decoder (Microsoft's `WAVE_FORMAT_ADPCM`) predicts each sample from the two
//! before by the predictor's coefficients, adds the nibble (signed) times the delta, and adapts
//! the delta by the nibble.
//!
//! Any bytes of whole blocks come back: the decoder's state is only the contexts of the model.

use aqr_common::arith::{ArithDecoder, ArithEncoder};

use crate::{Error, Result};

/// The coefficients of the predictors.
const COEF: [(i32, i32); 7] = [(256, 0), (512, -256), (0, 0), (192, 64), (240, 0), (460, -208), (392, -232)];
/// The delta's adaptation by nibble.
const ADAPT: [i32; 16] = [230, 230, 230, 230, 307, 409, 512, 614, 768, 614, 512, 409, 307, 230, 230, 230];
/// The delta is held within these, for the contexts (a decoder's 16 bits would wrap).
const DELTA_MIN: i32 = 16;
const DELTA_MAX: i32 = 1 << 24;

/// The decoder's state of a channel: the coefficients, the delta, the two samples before.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
struct State {
    c1: i32,
    c2: i32,
    delta: i32,
    s1: i32,
    s2: i32,
}

impl State {
    fn of(predictor: u8, delta: i16, s1: i16, s2: i16) -> State {
        // (a predictor of no coefficients predicts silence)
        let (c1, c2) = COEF.get(predictor as usize).copied().unwrap_or((0, 0));
        State { c1, c2, delta: (delta as i32).clamp(DELTA_MIN, DELTA_MAX), s1: s1 as i32, s2: s2 as i32 }
    }

    /// The state after nibble `n`.
    #[inline]
    fn step(&mut self, n: u8) {
        let pred = (self.s1 * self.c1 + self.s2 * self.c2) >> 8;
        let e = if n >= 8 { n as i32 - 16 } else { n as i32 };
        let s = (pred + e * self.delta).clamp(-32768, 32767);
        (self.s2, self.s1) = (self.s1, s);
        self.delta = ((ADAPT[n as usize] * self.delta) >> 8).clamp(DELTA_MIN, DELTA_MAX);
    }
}

/// An adaptive bit: the probability of a 1 (16 bits) and how many bits it has seen; each bit
/// coded moves the probability a 2^-shift of the way to it, the shift growing with the count
/// ([`SHIFTS`]). It stays within 1 and 65535 (as aqr-ima's).
#[derive(Debug, Clone, Copy)]
struct Bit {
    p: u16,
    n: u8,
}

const SHIFTS: [u8; 16] = [1, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 4, 5, 5, 5, 6];

impl Default for Bit {
    fn default() -> Self {
        Bit { p: 1 << 15, n: 0 }
    }
}

impl Bit {
    #[inline(always)]
    fn update(&mut self, b: bool) {
        let (p, sh) = (self.p as i32, SHIFTS[self.n as usize] as u32);
        self.p = if b { p + ((65535 - p) >> sh) } else { p - ((p - 1) >> sh) } as u16;
        self.n += (self.n < 15) as u8;
    }
}

/// Codes decisions: the encoder codes `b` and returns it, the decoder returns what it decodes.
trait Coder {
    fn bit(&mut self, k: &mut Bit, b: bool) -> bool;
}

impl Coder for ArithEncoder {
    #[inline(always)]
    fn bit(&mut self, k: &mut Bit, b: bool) -> bool {
        self.encode(b, k.p as u32);
        k.update(b);
        b
    }
}

impl Coder for ArithDecoder<'_> {
    #[inline(always)]
    fn bit(&mut self, k: &mut Bit, _: bool) -> bool {
        let b = self.decode(k.p as u32);
        k.update(b);
        b
    }
}

/// Delta buckets of the contexts: the delta's bit length (16 to 2^24: 5 to 25), less 5, at most
/// 15; and one for a channel of no state yet.
const BUCKETS: usize = 17;
/// Nibble contexts: none before (16) or the nibble before.
const PREV: usize = 17;

fn bucket(s: Option<State>) -> usize {
    s.map_or(BUCKETS - 1, |s| ((32 - s.delta.leading_zeros()) as usize).saturating_sub(5).min(BUCKETS - 2))
}

/// A signed difference as its zigzag value's bit length (unary, by context) and its bits below
/// the leading one (by length and position): values of at most 18 bits.
#[derive(Clone)]
struct Num {
    len: [[Bit; 19]; BUCKETS],
    bits: [[Bit; 18]; 19],
}

impl Num {
    fn new() -> Num {
        Num { len: [[Bit::default(); 19]; BUCKETS], bits: [[Bit::default(); 18]; 19] }
    }

    /// Codes difference `v` (any where decoding) in context `b`, and returns it.
    fn code<C: Coder>(&mut self, c: &mut C, b: usize, v: i32) -> i32 {
        let want = (v << 1 ^ v >> 31) as u32;
        let len = 32 - want.leading_zeros();
        let mut n = 0;
        while n < 18 && c.bit(&mut self.len[b][n as usize], n < len) {
            n += 1;
        }
        let mut z = if n == 0 { 0 } else { 1u32 };
        for i in (0..n.saturating_sub(1)).rev() {
            z = z << 1 | c.bit(&mut self.bits[n as usize][i as usize], want >> i & 1 == 1) as u32;
        }
        (z >> 1) as i32 ^ -((z & 1) as i32)
    }
}

/// The adaptive bits.
struct Model {
    /// The predictor: as the channel's in the block before (by whether there was one), else a
    /// tree of 8 bits.
    same_predictor: [Bit; 2],
    predictor: [Bit; 256],
    /// The delta against the channel's at the end of the block before; sample 2 against the
    /// samples before it, extrapolated; sample 1 against sample 2 and the one before it.
    delta: Num,
    s2: Num,
    s1: Num,
    /// Nibbles: a tree of 4 bits, by bucket, the channel's nibble before, and the nibble before
    /// (another channel's: of the same sample) or none.
    nibble: Vec<Bit>,
}

impl Model {
    fn new() -> Model {
        Model {
            same_predictor: [Bit::default(); 2],
            predictor: [Bit::default(); 256],
            delta: Num::new(),
            s2: Num::new(),
            s1: Num::new(),
            nibble: vec![Bit::default(); BUCKETS * PREV * PREV * 16],
        }
    }

    /// A value of `n` bits as a tree of counters `t` (of `2^n` of them), most significant first.
    #[inline(always)]
    fn tree<C: Coder>(c: &mut C, t: &mut [Bit], n: u32, v: u32) -> u32 {
        let mut node = 1usize;
        for i in (0..n).rev() {
            let b = c.bit(&mut t[node], v >> i & 1 == 1);
            node = node * 2 + b as usize;
        }
        (node - (1 << n)) as u32
    }
}

/// The layout of blocks: channels, and bytes per block (a header of 7 bytes per channel, then
/// the nibbles).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    pub channels: usize,
    pub block: usize,
}

impl Layout {
    /// A layout of 1 to 8 channels, of blocks of a nibble of each channel at least past their
    /// headers, and of whole samples (as many nibbles of each channel).
    pub fn new(channels: usize, block: usize) -> Option<Layout> {
        let ok = (1..=8).contains(&channels)
            && block > 7 * channels
            && block <= 1 << 20
            && (2 * (block - 7 * channels)).is_multiple_of(channels);
        ok.then_some(Layout { channels, block })
    }

    /// Nibbles per block, of all channels.
    fn nibbles(&self) -> usize {
        2 * (self.block - 7 * self.channels)
    }
}

/// Bytes of an `Option<State>`, an `Option<u8>` and a [`Bit`] on WebAssembly, for the plan.
const STATE_BYTES: u64 = 24;
const PREDICTOR_BYTES: u64 = 2;
const BIT_BYTES: u64 = 4;
#[cfg(target_family = "wasm")]
const _: () = assert!(
    size_of::<Option<State>>() as u64 == STATE_BYTES
        && size_of::<Option<u8>>() as u64 == PREDICTOR_BYTES
        && size_of::<Bit>() as u64 == BIT_BYTES
);

/// What [`decode`] takes of an [`aqr_api::stack`] at most, of an output of `len` bytes of `l`:
/// the output, and on top of it what coding takes (`code`): the model's nibbles, each channel's
/// state, predictor and nibble before, and a block. (The rest of the model is on the stack.)
pub fn decode_plan(l: Layout, len: u64) -> u64 {
    use aqr_api::stack::block;
    let ch = l.channels as u64;
    let model = block(BIT_BYTES * (BUCKETS * PREV * PREV * 16) as u64);
    block(len) + model + block(STATE_BYTES * ch) + block(PREDICTOR_BYTES * ch) + block(ch) + block(l.block as u64)
}

fn i16_at(b: &[u8], at: usize) -> i16 {
    i16::from_le_bytes([b[at], b[at + 1]])
}

/// Codes the blocks of `d` (whole blocks of `l`) with `c`, writing them to `out` (decoding;
/// encoding gives `d` back).
fn code<C: Coder>(cod: &mut C, l: Layout, d: Option<&[u8]>, blocks: usize, out: &mut Vec<u8>) {
    let (ch, bs, n) = (l.channels, l.block, l.nibbles());
    let mut m = Model::new();
    // each channel's state at the end of the block before, and its predictor there
    let mut end: Vec<Option<State>> = vec![None; ch];
    let mut predictor: Vec<Option<u8>> = vec![None; ch];
    let mut prev = vec![0u8; ch];
    let mut block = vec![0u8; bs];
    for k in 0..blocks {
        let src = d.map(|d| &d[k * bs..(k + 1) * bs]);
        let mut state = [State::of(0, 0, 0, 0); 8];
        for c in 0..ch {
            // the predictor
            let want = src.map_or(0, |s| s[c]);
            let had = predictor[c];
            let p = match cod.bit(&mut m.same_predictor[had.is_some() as usize], had == Some(want) && src.is_some()) {
                true => had.unwrap_or(0),
                false => Model::tree(cod, &mut m.predictor, 8, want as u32) as u8,
            };
            // the delta, against the end's
            let e = end[c];
            let b = bucket(e);
            // (the end's within 16 bits, as a header's: a difference of 17 bits at most)
            let base = e.map_or(0, |e| e.delta.min(32767));
            let want = src.map_or(0, |s| i16_at(s, ch + 2 * c) as i32);
            let delta = (base + m.delta.code(cod, b, want - base)) as i16;
            // sample 2 (the first), against the samples before it extrapolated; sample 1 (the
            // second) against sample 2 and the one before it, extrapolated
            let hb = bucket(Some(State::of(0, delta, 0, 0)));
            let (e1, e2) = e.map_or((0, 0), |e| (e.s1, e.s2));
            let p2 = (2 * e1 - e2).clamp(-32768, 32767);
            let want = src.map_or(0, |s| i16_at(s, 5 * ch + 2 * c) as i32);
            let s2 = (p2 + m.s2.code(cod, hb, want - p2)) as i16;
            let p1 = (2 * s2 as i32 - e1).clamp(-32768, 32767);
            let want = src.map_or(0, |s| i16_at(s, 3 * ch + 2 * c) as i32);
            let s1 = (p1 + m.s1.code(cod, hb, want - p1)) as i16;
            block[c] = p;
            block[ch + 2 * c..ch + 2 * c + 2].copy_from_slice(&delta.to_le_bytes());
            block[3 * ch + 2 * c..3 * ch + 2 * c + 2].copy_from_slice(&s1.to_le_bytes());
            block[5 * ch + 2 * c..5 * ch + 2 * c + 2].copy_from_slice(&s2.to_le_bytes());
            predictor[c] = Some(p);
            state[c] = State::of(p, delta, s1, s2);
        }
        // the nibbles, each channel's in turn
        let mut before = (PREV - 1) as u8;
        for (j, c) in (0..n).map(|j| (j, j % ch)) {
            let at = 7 * ch + j / 2;
            let want = src.map_or(0, |s| if j % 2 == 0 { s[at] >> 4 } else { s[at] & 15 });
            let mine = if j < ch { PREV - 1 } else { prev[c] as usize };
            let other = if c > 0 { before as usize } else { PREV - 1 };
            let base = ((bucket(Some(state[c])) * PREV + mine) * PREV + other) * 16;
            let v = Model::tree(cod, &mut m.nibble[base..base + 16], 4, want as u32) as u8;
            block[at] = if j % 2 == 0 { (block[at] & 0x0F) | v << 4 } else { (block[at] & 0xF0) | v };
            state[c].step(v);
            (prev[c], before) = (v, v);
        }
        for c in 0..ch {
            end[c] = Some(state[c]);
        }
        out.extend_from_slice(&block);
    }
}

/// The record of the whole blocks of `l` at the start of `d`, and the bytes they take.
pub fn encode(l: Layout, d: &[u8]) -> (usize, Vec<u8>) {
    let blocks = d.len() / l.block;
    let mut c = ArithEncoder::new(Vec::new());
    let mut out = Vec::with_capacity(blocks * l.block);
    code(&mut c, l, Some(d), blocks, &mut out);
    debug_assert!(out == d[..blocks * l.block]);
    (blocks * l.block, c.finish())
}

/// The blocks of `record`, `len` bytes (a multiple of the block's).
pub fn decode(l: Layout, record: &[u8], len: usize) -> Result<Vec<u8>> {
    if !len.is_multiple_of(l.block) {
        return Err(Error("not whole blocks"));
    }
    let mut c = ArithDecoder::new(record);
    let mut out = Vec::with_capacity(len);
    code(&mut c, l, None, len / l.block, &mut out);
    if c.overrun() {
        return Err(Error("corrupt record"));
    }
    Ok(out)
}
