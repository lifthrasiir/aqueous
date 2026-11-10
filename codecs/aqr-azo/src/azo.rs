//! AZO streams: their blocks decoded into their data and the tokens of their parse, and encoded
//! again from them (`FORMAT.md`, "AZO streams"; the spec's Appendix A).

use crate::{Error, Result, corrupt};

/// Bytes of a stream's header: the version (`'1'`) and the flags (bit 0: the x86 filter).
pub const HEADER: usize = 2;
pub const VERSION: u8 = b'1';
/// Bytes of a block's header: its data's and its code's sizes and their XOR, big-endian.
pub const BLOCK_HEADER: usize = 12;

pub const MIN_LENGTH: u32 = 2;
pub const MIN_DIST: u32 = 1;
const HISTORY: usize = 128;

/// One step of a block's parse after its first byte (a literal).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Token {
    /// A literal: the data's byte.
    Literal,
    /// A remembered match again, by its place in the two recent places taken (0, 1).
    ReuseRecent(u8),
    /// A remembered match again, by its place in the history (0 to 127).
    Reuse(u8),
    /// A match of one of the two recent distances (0, 1), of a length.
    Near(u8, u32),
    /// A match of a distance and a length.
    Far(u32, u32),
}

/// The base and the extra bits of each code of lengths and of distances.
struct Codes {
    len_base: [u32; 129],
    len_extra: [u32; 128],
    dist_base: [u32; 129],
    dist_extra: [u32; 128],
}

const fn codes() -> Codes {
    let mut c = Codes { len_base: [0; 129], len_extra: [0; 128], dist_base: [0; 129], dist_extra: [0; 128] };
    c.len_base[0] = MIN_LENGTH;
    c.dist_base[0] = MIN_DIST;
    let mut i = 0;
    while i < 128 {
        c.len_extra[i] = if i < 32 { 0 } else { (i as u32 - 32) / 8 };
        c.dist_extra[i] = if i < 16 { 0 } else { (i as u32 - 16) / 4 };
        c.len_base[i + 1] = c.len_base[i] + (1 << c.len_extra[i]);
        c.dist_base[i + 1] = c.dist_base[i] + (1 << c.dist_extra[i]);
        i += 1;
    }
    c
}

static CODES: Codes = codes();

/// The longest match and the farthest distance.
pub const MAX_LENGTH: u32 = codes().len_base[128] - 1;
pub const MAX_DIST: u32 = codes().dist_base[128] - 1;

/// The code of `v` in the table `base` (`v` at least its first).
fn code_of(base: &[u32; 129], v: u32) -> usize {
    base.partition_point(|&b| b <= v) - 1
}

/// What codes a stream's symbols: the decoder (which returns what it reads, and ignores the
/// value given) or the encoder (which writes the value given, and returns it).
pub trait Coder {
    /// A bit: 0 with probability `cum` / 2^`total`.
    fn boolean(&mut self, cum: u32, total: u32, bit: u32) -> u32;
    /// A value of `bits` bits, of even probabilities.
    fn uniform(&mut self, bits: u32, value: u32) -> u32;
}

const MSB: u64 = 0x8000_0000;
const SMSB: u64 = 0x4000_0000;
const MASK: u64 = 0xFFFF_FFFF;

/// The range decoder of a block's code.
pub struct Decoder<'a> {
    code: &'a [u8],
    /// Bits read.
    at: usize,
    low: u64,
    up: u64,
    tag: u64,
}

impl<'a> Decoder<'a> {
    pub fn new(code: &'a [u8]) -> Self {
        let mut d = Decoder { code, at: 0, low: 0, up: MASK, tag: 0 };
        for _ in 0..32 {
            d.tag = d.tag << 1 | d.bit();
        }
        d
    }

    /// The next bit, 0 past the end.
    fn bit(&mut self) -> u64 {
        let b = self.code.get(self.at >> 3).map_or(0, |&b| (b >> (7 - (self.at & 7))) & 1);
        self.at += 1;
        b as u64
    }

    /// Bits read, past the end too.
    pub fn bits_read(&self) -> usize {
        self.at
    }

    fn rescale(&mut self) {
        while (self.low ^ self.up) & MSB == 0 {
            self.tag = (self.tag << 1 & MASK) | self.bit();
            self.low = self.low << 1 & MASK;
            self.up = (self.up << 1 & MASK) | 1;
        }
        while self.low & SMSB != 0 && self.up & SMSB == 0 {
            self.tag = ((self.tag << 1 | self.bit()) ^ MSB) & MASK;
            self.low = self.low << 1 & (MASK >> 1);
            self.up = (self.up << 1 | 1 | MSB) & MASK;
        }
    }
}

impl Coder for Decoder<'_> {
    fn boolean(&mut self, cum: u32, total: u32, _: u32) -> u32 {
        let t = (self.up - self.low + 1) >> total;
        let v = (self.tag.wrapping_sub(self.low)) / t;
        let bit = if v >= cum as u64 {
            self.low += t * cum as u64;
            1
        } else {
            self.up = self.low + t * cum as u64 - 1;
            0
        };
        self.rescale();
        bit
    }

    fn uniform(&mut self, bits: u32, _: u32) -> u32 {
        let t = (self.up - self.low + 1) >> bits;
        let v = ((self.tag.wrapping_sub(self.low)) / t).min((1 << bits) - 1);
        self.up = self.low + t * (v + 1) - 1;
        self.low += t * v;
        self.rescale();
        v as u32
    }
}

/// The range encoder of a block's code.
pub struct Encoder {
    out: Vec<u8>,
    /// Where the code starts in `out` (bytes before it are another's).
    start: usize,
    /// Bits in the last byte of `out` (0 to 7; 0 where it is full or there is none).
    used: u32,
    low: u64,
    up: u64,
    /// Bits put off by the rescaling of the middle (each the opposite of the next bit out).
    pending: u64,
}

impl Default for Encoder {
    fn default() -> Self {
        Encoder::onto(Vec::new())
    }
}

impl Encoder {
    /// An encoder writing its code onto the end of `out`, which [`Encoder::finish`] gives back.
    pub fn onto(out: Vec<u8>) -> Self {
        Encoder { start: out.len(), out, used: 0, low: 0, up: MASK, pending: 0 }
    }

    fn put(&mut self, bit: u64) {
        if self.used == 0 {
            self.out.push(0);
        }
        *self.out.last_mut().unwrap() |= (bit as u8) << (7 - self.used);
        self.used = (self.used + 1) & 7;
    }

    fn put_pending(&mut self, bit: u64) {
        self.put(bit);
        for _ in 0..std::mem::take(&mut self.pending) {
            self.put(bit ^ 1);
        }
    }

    fn rescale(&mut self) {
        while (self.low ^ self.up) & MSB == 0 {
            self.put_pending(self.low >> 31);
            self.low = self.low << 1 & MASK;
            self.up = (self.up << 1 & MASK) | 1;
        }
        while self.low & SMSB != 0 && self.up & SMSB == 0 {
            self.pending += 1;
            self.low = self.low << 1 & (MASK >> 1);
            self.up = (self.up << 1 | 1 | MSB) & MASK;
        }
    }

    /// The code, ended as ALZip ends it: one more bit put off, bit 30 of `low` and the bits put
    /// off, zeros to a byte; then the zero bytes at the end dropped (the decoder reads zeros past
    /// the end). Gives back the vector it wrote onto, the code at its end.
    pub fn finish(mut self) -> Vec<u8> {
        self.pending += 1;
        let b = (self.low >> 30) & 1;
        self.put_pending(b);
        while self.out.len() > self.start && self.out.last() == Some(&0) {
            self.out.pop();
        }
        self.out
    }
}

impl Coder for Encoder {
    fn boolean(&mut self, cum: u32, total: u32, bit: u32) -> u32 {
        let t = (self.up - self.low + 1) >> total;
        if bit != 0 {
            self.low += t * cum as u64;
        } else {
            self.up = self.low + t * cum as u64 - 1;
        }
        self.rescale();
        bit
    }

    fn uniform(&mut self, bits: u32, v: u32) -> u32 {
        let t = (self.up - self.low + 1) >> bits;
        self.up = self.low + t * (v as u64 + 1) - 1;
        self.low += t * v as u64;
        self.rescale();
        v
    }
}

/// An adaptive bit with a context of its last 8 bits (the spec's BoolState).
#[derive(Clone)]
struct BoolState {
    state: usize,
    prob: [u32; 256],
}

impl BoolState {
    fn new() -> Self {
        BoolState { state: 0, prob: [2048; 256] }
    }

    fn code(&mut self, c: &mut impl Coder, bit: u32) -> u32 {
        let p = &mut self.prob[self.state];
        let bit = c.boolean(*p, 12, bit);
        if bit == 0 {
            *p += (4096 - *p) >> 6;
        } else {
            *p -= *p >> 6;
        }
        self.state = (self.state << 1 | bit as usize) & 255;
        bit
    }
}

/// The adaptive bits of a value of `bits` bits, a binary tree (the spec's EntropyBitProb).
fn tree_code(prob: &mut [u32], bits: u32, c: &mut impl Coder, value: u32) -> u32 {
    let (mut v, mut node) = (0, 1);
    for i in (0..bits).rev() {
        let p = &mut prob[node];
        let bit = c.boolean(*p, 10, value >> i & 1);
        if bit == 0 {
            *p += (1024 - *p) >> 4;
        } else {
            *p -= *p >> 4;
        }
        v |= bit << i;
        node = node << 1 | bit as usize;
    }
    v
}

/// [`tree_code`] adapted to `value` with nothing coded.
fn tree_update(prob: &mut [u32], bits: u32, value: u32) {
    let mut node = 1;
    for i in (0..bits).rev() {
        let bit = value >> i & 1;
        let p = &mut prob[node];
        if bit == 0 {
            *p += (1024 - *p) >> 4;
        } else {
            *p -= *p >> 4;
        }
        node = node << 1 | bit as usize;
    }
}

/// Which of two trees gave `value` the more probability: 1 the first, -1 the second, 0 neither.
fn compare(a: &[u32], b: &[u32], bits: u32, value: u32) -> i32 {
    let (mut pa, mut pb, mut node) = (1u32, 1u32, 1);
    for i in (0..bits).rev() {
        let v = value >> i & 1;
        if (pa | pb) & (1023 << 22) != 0 {
            pa >>= 10;
            pb >>= 10;
        }
        let (x, y) = (a[node], b[node]);
        pa = pa.wrapping_mul(if v != 0 { 1024 - x } else { x });
        pb = pb.wrapping_mul(if v != 0 { 1024 - y } else { y });
        node = node << 1 | v as usize;
    }
    (pa > pb) as i32 - (pa < pb) as i32
}

/// Two trees of each context, a fine and a coarse (the context shifted), coding by the one that
/// predicted better so far (the spec's PredictProb).
#[derive(Clone)]
struct Predict {
    bits: u32,
    shift: u32,
    fine: Vec<u32>,
    coarse: Vec<u32>,
    lucky: Vec<i32>,
}

impl Predict {
    /// Bytes of the vectors of a predictor, in the order [`Predict::new`] makes them.
    const fn bytes(keys: usize, bits: u32, shift: u32) -> [u64; 3] {
        let n = 1u64 << bits;
        [4 * keys as u64 * n, 4 * (keys >> shift) as u64 * n, 4 * keys as u64]
    }

    fn new(keys: usize, bits: u32, shift: u32) -> Self {
        let n = 1 << bits;
        Predict { bits, shift, fine: vec![512; keys * n], coarse: vec![512; (keys >> shift) * n], lucky: vec![0; keys] }
    }

    fn code(&mut self, c: &mut impl Coder, key: usize, value: u32) -> u32 {
        let n = 1 << self.bits;
        let fine = &mut self.fine[key * n..][..n];
        let coarse = &mut self.coarse[(key >> self.shift) * n..][..n];
        let v = if self.lucky[key] >= 0 {
            let v = tree_code(fine, self.bits, c, value);
            tree_update(coarse, self.bits, v);
            v
        } else {
            let v = tree_code(coarse, self.bits, c, value);
            tree_update(fine, self.bits, v);
            v
        };
        self.lucky[key] += compare(fine, coarse, self.bits, v);
        v
    }
}

const ALPHA: (usize, u32, u32) = (256, 8, 5);
const LENGTH: (usize, u32, u32) = (128, 7, 4);

/// Bytes of the vectors of a [`Model`], in the order [`Model::default`] makes them.
pub const MODEL_BYTES: [u64; 10] = {
    let (a, l) = (Predict::bytes(ALPHA.0, ALPHA.1, ALPHA.2), Predict::bytes(LENGTH.0, LENGTH.1, LENGTH.2));
    [8, 512, 8, 512, a[0], a[1], a[2], l[0], l[1], l[2]]
};

/// The models of a block.
#[derive(Clone)]
pub struct Model {
    flag: BoolState,
    reuse: BoolState,
    reuse_short: BoolState,
    reuse_recent: Vec<u32>,
    reuse_index: Vec<u32>,
    dist_short: BoolState,
    dist_recent: Vec<u32>,
    dist_code: Vec<u32>,
    alpha: Predict,
    length: Predict,
    pub history: History,
}

impl Default for Model {
    fn default() -> Self {
        Model {
            flag: BoolState::new(),
            reuse: BoolState::new(),
            reuse_short: BoolState::new(),
            reuse_recent: vec![512; 2],
            reuse_index: vec![512; 128],
            dist_short: BoolState::new(),
            dist_recent: vec![512; 2],
            dist_code: vec![512; 128],
            alpha: Predict::new(ALPHA.0, ALPHA.1, ALPHA.2),
            length: Predict::new(LENGTH.0, LENGTH.1, LENGTH.2),
            history: History::default(),
        }
    }
}

/// The lists of a block's matches the tokens refer to (the spec's HistoryLists): the two recent
/// distances, the two recent places taken of the history, and the history of matches (their
/// lengths and the positions they were at).
#[derive(Clone)]
pub struct History {
    pub recent_dist: [u32; 2],
    pub recent_index: [u32; 2],
    pub lengths: [u32; HISTORY],
    pub places: [u32; HISTORY],
}

impl Default for History {
    fn default() -> Self {
        History {
            recent_dist: [MIN_DIST, MIN_DIST + 1],
            recent_index: [0, 1],
            lengths: std::array::from_fn(|i| MIN_LENGTH + i as u32),
            places: [0; HISTORY],
        }
    }
}

impl History {
    /// The match of token `t` at `pos` (its distance, which wraps where the place is not before
    /// `pos`, and its length; None for a literal), the lists updated.
    pub fn apply(&mut self, pos: u32, t: Token) -> Option<(u32, u32)> {
        let index = match t {
            Token::Literal => return None,
            Token::ReuseRecent(k) => take(&mut self.recent_index, k as usize & 1),
            Token::Reuse(k) => {
                add(&mut self.recent_index, k as u32 & 127);
                k as u32 & 127
            }
            Token::Near(k, n) => {
                let d = take(&mut self.recent_dist, k as usize & 1);
                add(&mut self.lengths, n);
                add(&mut self.places, pos);
                return Some((d, n));
            }
            Token::Far(d, n) => {
                add(&mut self.recent_dist, d);
                add(&mut self.lengths, n);
                add(&mut self.places, pos);
                return Some((d, n));
            }
        };
        let len = take(&mut self.lengths, index as usize);
        let place = take(&mut self.places, index as usize);
        Some((pos.wrapping_sub(place), len))
    }
}

/// `list[i]` moved to the front.
fn take<T: Copy>(list: &mut [T], i: usize) -> T {
    let v = list[i];
    list.copy_within(..i, 1);
    list[0] = v;
    v
}

/// `v` put at the front, the last dropped.
fn add<T: Copy>(list: &mut [T], v: T) {
    list.copy_within(..list.len() - 1, 1);
    list[0] = v;
}

/// A match decoded or to encode: its distance and length.
struct Match {
    dist: u32,
    len: u32,
}

impl Model {
    fn literal(&mut self, c: &mut impl Coder, prev: u8, byte: u8) -> u8 {
        self.alpha.code(c, prev as usize, byte as u32) as u8
    }

    /// The token at `pos` (after the first byte): `t` coded, or decoded where `c` decodes.
    fn token(&mut self, c: &mut impl Coder, pos: u32, t: Token) -> (Token, Option<Match>) {
        let is = |b: bool| b as u32;
        if self.flag.code(c, is(t != Token::Literal)) == 0 {
            return (Token::Literal, None);
        }
        let reuse = matches!(t, Token::ReuseRecent(_) | Token::Reuse(_));
        let t = if self.reuse.code(c, is(reuse)) == 1 {
            if self.reuse_short.code(c, is(matches!(t, Token::ReuseRecent(_)))) == 1 {
                let k = if let Token::ReuseRecent(k) = t { k as u32 } else { 0 };
                Token::ReuseRecent(tree_code(&mut self.reuse_recent, 1, c, k) as u8)
            } else {
                let k = if let Token::Reuse(k) = t { k as u32 } else { 0 };
                Token::Reuse(tree_code(&mut self.reuse_index, 7, c, k) as u8)
            }
        } else {
            let (dist, len) = match t {
                Token::Near(_, n) => (0, n),
                Token::Far(d, n) => (d, n),
                _ => (0, 0),
            };
            let (near, dist) = if self.dist_short.code(c, is(matches!(t, Token::Near(..)))) == 1 {
                let k = if let Token::Near(k, _) = t { k as u32 } else { 0 };
                let k = tree_code(&mut self.dist_recent, 1, c, k);
                (Some(k as u8), self.history.recent_dist[k as usize])
            } else {
                let code = tree_code(&mut self.dist_code, 7, c, code_of(&CODES.dist_base, dist.max(1)) as u32) as usize;
                let extra = CODES.dist_extra[code];
                let mut d = CODES.dist_base[code];
                if extra > 0 {
                    d += c.uniform(extra, dist.wrapping_sub(d));
                }
                (None, d)
            };
            let key = code_of(&CODES.dist_base, dist.clamp(MIN_DIST, MAX_DIST));
            let code = self.length.code(c, key, code_of(&CODES.len_base, len.max(MIN_LENGTH)) as u32) as usize;
            let extra = CODES.len_extra[code];
            let mut n = CODES.len_base[code];
            if extra > 0 {
                n += c.uniform(extra, len.wrapping_sub(n));
            }
            match near {
                Some(k) => Token::Near(k, n),
                None => Token::Far(dist, n),
            }
        };
        let (dist, len) = self.history.apply(pos, t).expect("a match");
        (t, Some(Match { dist, len }))
    }
}

/// A block decoded: its data (the filter's output, before it is undone) and tokens.
pub struct Block {
    pub data: Vec<u8>,
    pub tokens: Vec<Token>,
    /// The distance and length of each token (0 and 1 for a literal).
    pub spans: Vec<(u32, u32)>,
    /// Bits the decoder read.
    pub bits: usize,
}

/// Decodes the code of a block of `size` bytes of data with `model`.
pub fn decode_block(code: &[u8], size: usize, model: &mut Model) -> Result<Block> {
    let mut c = Decoder::new(code);
    let mut data = Vec::with_capacity(size);
    let mut tokens = Vec::new();
    let mut spans = Vec::new();
    if size > 0 {
        data.push(model.literal(&mut c, 0, 0));
    }
    while data.len() < size {
        let pos = data.len();
        let (t, m) = model.token(&mut c, pos as u32, Token::Literal);
        match m {
            None => {
                let b = model.literal(&mut c, data[pos - 1], 0);
                data.push(b);
                spans.push((0, 1));
            }
            Some(Match { dist, len }) => {
                spans.push((dist, len));
                let d = dist as usize;
                if d == 0 || d > pos || pos + len as usize > size {
                    return Err(corrupt());
                }
                for k in 0..len as usize {
                    data.push(data[pos - d + k]);
                }
            }
        }
        tokens.push(t);
        if c.bits_read() > code.len() * 8 + 64 {
            return Err(Error("code overrun"));
        }
    }
    Ok(Block { data, tokens, spans, bits: c.bits_read() })
}

/// A block's tokens encoded one by one as they come (the encoding of [`encode_block`]).
pub struct BlockEncoder<'a> {
    data: &'a [u8],
    model: &'a mut Model,
    c: Encoder,
    pos: usize,
}

impl<'a> BlockEncoder<'a> {
    /// The encoding of the block of `data` (before the x86 filter) with `model` onto `c`: its
    /// first byte put.
    pub fn new(data: &'a [u8], model: &'a mut Model, mut c: Encoder) -> Self {
        if let Some(&b) = data.first() {
            model.literal(&mut c, 0, b);
        }
        BlockEncoder { data, model, c, pos: data.len().min(1) }
    }

    /// The next token encoded.
    pub fn token(&mut self, t: Token) -> Result<()> {
        if self.pos >= self.data.len() {
            return Err(corrupt());
        }
        let (_, m) = self.model.token(&mut self.c, self.pos as u32, t);
        match m {
            None => {
                self.model.literal(&mut self.c, self.data[self.pos - 1], self.data[self.pos]);
                self.pos += 1;
            }
            Some(m) => self.pos += m.len as usize,
        }
        Ok(())
    }

    /// The encoder, ended, with its code: where the tokens covered the data.
    pub fn finish(self) -> Result<Vec<u8>> {
        if self.pos != self.data.len() {
            return Err(corrupt());
        }
        Ok(self.c.finish())
    }
}

/// Encodes the block of `data` (before the x86 filter) parsed as `tokens` with `model`.
pub fn encode_block(data: &[u8], tokens: &[Token], model: &mut Model) -> Result<Vec<u8>> {
    let mut e = BlockEncoder::new(data, model, Encoder::default());
    for &t in tokens {
        e.token(t)?;
    }
    e.finish()
}

/// The x86 filter of a block's data as the decoder applies it: the addresses of calls and jumps
/// made relative.
pub fn unfilter(d: &mut [u8]) {
    filter_with(d, false);
}

/// The x86 filter as the encoder applies it: the addresses made absolute.
pub fn filter(d: &mut [u8]) {
    filter_with(d, true);
}

fn filter_with(d: &mut [u8], absolute: bool) {
    if d.len() < 5 {
        return;
    }
    let mut i = 0;
    while i < d.len() - 4 {
        if d[i] == 0xE8 || d[i] == 0xE9 {
            // (the five bytes are skipped whether the address is converted or not)
            if d[i + 4] == 0 || d[i + 4] == 0xFF {
                let a = u32::from_le_bytes(d[i + 1..i + 5].try_into().unwrap());
                let a = if absolute { a.wrapping_add(i as u32) } else { a.wrapping_sub(i as u32) };
                let a = if a >> 24 & 1 != 0 { a | 0xFF00_0000 } else { a & 0x00FF_FFFF };
                d[i + 1..i + 5].copy_from_slice(&a.to_le_bytes());
            }
            i += 5;
        } else {
            i += 1;
        }
    }
}

/// A block of a stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockAt {
    /// Bytes of its data.
    pub size: usize,
    /// Where its code starts and ends in the stream.
    pub code: std::ops::Range<usize>,
}

impl BlockAt {
    /// Whether it is stored: its code is its data.
    pub fn stored(&self) -> bool {
        self.code.len() == self.size
    }
}

/// A stream's structure: its x86 filter flag and its blocks, up to its end (a block of sizes 0),
/// and where it ends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stream {
    pub x86: bool,
    pub blocks: Vec<BlockAt>,
    pub end: usize,
}

/// The structure of the stream at the start of `d`.
pub fn parse(d: &[u8]) -> Result<Stream> {
    let &[v, flags, ..] = d else { return Err(corrupt()) };
    if v != VERSION || flags > 1 {
        return Err(Error("not an AZO stream"));
    }
    let mut at = HEADER;
    let mut blocks = Vec::new();
    loop {
        let h = d.get(at..at + BLOCK_HEADER).ok_or_else(corrupt)?;
        let be = |k: usize| u32::from_be_bytes(h[k..k + 4].try_into().unwrap());
        let (size, csize, check) = (be(0), be(4), be(8));
        if size ^ csize != check {
            return Err(Error("block header's check"));
        }
        at += BLOCK_HEADER;
        if size == 0 && csize == 0 {
            break;
        }
        let end = at.checked_add(csize as usize).filter(|&e| e <= d.len()).ok_or_else(corrupt)?;
        blocks.push(BlockAt { size: size as usize, code: at..end });
        at = end;
    }
    Ok(Stream { x86: flags & 1 != 0, blocks, end: at })
}

/// A stream decoded: its data (after the x86 filter) and each block's tokens (none for a stored
/// block).
pub struct Decoded {
    pub stream: Stream,
    pub data: Vec<u8>,
    pub tokens: Vec<Option<Vec<Token>>>,
}

/// Decodes the stream at the start of `d`, of at most `limit` bytes of data.
pub fn decode(d: &[u8], limit: usize) -> Result<Decoded> {
    let stream = parse(d)?;
    let mut data = Vec::new();
    let mut tokens = Vec::new();
    for b in &stream.blocks {
        if data.len() + b.size > limit {
            return Err(Error("over the limit"));
        }
        let code = &d[b.code.clone()];
        let mut x = if b.stored() {
            tokens.push(None);
            code.to_vec()
        } else {
            let blk = decode_block(code, b.size, &mut Model::default())?;
            tokens.push(Some(blk.tokens));
            blk.data
        };
        if stream.x86 {
            unfilter(&mut x);
        }
        data.extend_from_slice(&x);
    }
    Ok(Decoded { stream, data, tokens })
}
