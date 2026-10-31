//! LZX streams, of two [`Variant`]s, read into their [`Parse`], everything their bits say, and
//! written back from it:
//!
//! - chunks as WIM files hold them (`FORMAT.md` §3): each chunk of at most 32 KiB compressed on
//!   its own, with a window of 32 KiB, its data translated by e8 with a size of 12000000;
//! - streams as CAB and CHM files hold them: a header telling the e8 translation's size (or
//!   none), then blocks over frames of 32 KiB of data, the bitstream aligned to a word after each
//!   frame (where a CAB file cuts it into its data blocks), with a window of 32 KiB to 2 MiB. Each
//!   frame's data is translated by e8 on its own, at its place in the stream.
//!
//! The bitstream is 16-bit little-endian words, read from their top bit. A block is a type (3
//! bits), a size (WIM's: a bit telling 32768, else 16 bits; CAB's: 24 bits), for aligned blocks
//! the aligned tree, then the main and length trees (coded against the block before's by a
//! pretree each) and the tokens; or, for an uncompressed block, its bytes after the stream is
//! aligned to a word.

/// Bytes of a chunk, and of its window; and of a frame.
pub const WINDOW: usize = 32768;
pub const FRAME: usize = 32768;
/// Position slots of a window of 32 KiB, and of 2 MiB.
pub const SLOTS: usize = 30;
pub const MAX_SLOTS: usize = 50;
pub const MAIN: usize = 256 + 8 * SLOTS;
pub const MAIN_MAX: usize = 256 + 8 * MAX_SLOTS;
pub const LEN: usize = 249;
pub const ALIGNED: usize = 8;
pub const PRE: usize = 20;
/// The e8 translation's size.
pub const E8_SIZE: i32 = 12_000_000;
pub const MIN_MATCH: usize = 2;
pub const MAX_MATCH: usize = 257;
/// Code lengths are at most this.
pub const MAX_LEN: u8 = 16;

pub const VERBATIM: u8 = 1;
pub const ALIGNED_BLOCK: u8 = 2;
pub const UNCOMPRESSED: u8 = 3;

/// Extra bits of each position slot (at most 17).
pub const EXTRA: [u8; MAX_SLOTS] = {
    let mut e = [0; MAX_SLOTS];
    let mut s = 4;
    while s < MAX_SLOTS {
        e[s] = if s < 36 { ((s - 2) / 2) as u8 } else { 17 };
        s += 1;
    }
    e
};

/// The smallest formatted offset of each position slot.
pub const BASE: [u32; MAX_SLOTS] = {
    let mut b = [0; MAX_SLOTS];
    let mut s = 1;
    while s < MAX_SLOTS {
        b[s] = b[s - 1] + (1 << EXTRA[s - 1]);
        s += 1;
    }
    b
};

/// The position slot of formatted offset `f` (at least 3).
pub fn slot(f: u32) -> usize {
    BASE.partition_point(|&b| b <= f) - 1
}

/// How a stream is laid out (see the module's documentation).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Variant {
    /// The window's bytes, as a power of two: 15 to 21.
    pub window_bits: u8,
    /// CAB's and CHM's streams: a header, block sizes of 24 bits, frames. Else WIM's chunks.
    pub cab: bool,
}

/// WIM's chunks.
pub const WIM: Variant = Variant { window_bits: 15, cab: false };

impl Variant {
    /// A stream as CAB and CHM files hold them, of a window of `2^window_bits` bytes (None out of
    /// 15 to 21).
    pub fn cab(window_bits: u8) -> Option<Variant> {
        (15..=21).contains(&window_bits).then_some(Variant { window_bits, cab: true })
    }

    pub fn window(self) -> usize {
        1 << self.window_bits
    }

    /// The position slots.
    pub fn slots(self) -> usize {
        match self.window_bits {
            ..=19 => 2 * self.window_bits as usize,
            20 => 42,
            _ => 50,
        }
    }

    /// The main tree's symbols.
    pub fn main(self) -> usize {
        256 + 8 * self.slots()
    }

    /// The farthest a match reaches back.
    pub fn max_off(self) -> usize {
        self.window() - 3
    }
}

/// A token: a literal (the next byte of the data), or a match of `len` bytes `off` back, coded
/// as repeat `rep` where given (the offset in that slot of the three last ones).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tok {
    Lit,
    Match { len: u16, off: u32, rep: Option<u8> },
}

/// How a tree's code lengths are written: the pretree's lengths and its symbols, each with its
/// extra bits (the run of 17 and 18, the run and the symbol after 19).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TreeCode {
    pub pre: [u8; PRE],
    pub syms: Vec<PreSym>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PreSym {
    pub sym: u8,
    /// The run's bits (17: 4, 18: 5, 19: 1).
    pub extra: u8,
    /// After 19: the pretree symbol of the value.
    pub then: u8,
}

/// A block: its type, size and how it says it, and by type its trees and tokens or its bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Block {
    pub kind: u8,
    pub size: usize,
    /// The size written as the bit telling 32768.
    pub default_size: bool,
    pub aligned: [u8; ALIGNED],
    /// The main tree's first 256 lengths, its other 240, the length tree's.
    pub trees: [TreeCode; 3],
    /// Tokens of the block (of [`Parse::toks`]).
    pub toks: usize,
    /// Uncompressed: the bits padding to a word (their number follows from where the stream
    /// is), the three repeat offsets written, and the pad byte after an odd size (none where
    /// the chunk ends there).
    pub align_bits: u16,
    pub reps: [u32; 3],
    pub pad_byte: Option<u8>,
}

impl Default for Block {
    fn default() -> Block {
        Block {
            kind: VERBATIM,
            size: 0,
            default_size: false,
            aligned: [0; ALIGNED],
            trees: Default::default(),
            toks: 0,
            align_bits: 0,
            reps: [0; 3],
            pad_byte: None,
        }
    }
}

/// A stream's parse: (CAB's) its header, its blocks and tokens, (CAB's) the bits padding each
/// frame but the last, then the bits padding its last word and whatever bytes follow.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Parse {
    pub e8: Option<u32>,
    pub blocks: Vec<Block>,
    pub toks: Vec<Tok>,
    pub frame_pad: Vec<u16>,
    pub tail_bits: u16,
    pub trailing: Vec<u8>,
}

struct BitR<'a> {
    d: &'a [u8],
    /// The next byte to load.
    p: usize,
    buf: u64,
    n: u32,
}

impl<'a> BitR<'a> {
    fn new(d: &'a [u8], p: usize) -> Self {
        BitR { d, p, buf: 0, n: 0 }
    }

    #[inline]
    fn fill(&mut self) {
        while self.n <= 48 {
            let w = match self.d.get(self.p..self.p + 2) {
                Some(w) => u16::from_le_bytes([w[0], w[1]]),
                None => 0,
            };
            self.p += 2;
            self.buf |= (w as u64) << (48 - self.n);
            self.n += 16;
        }
    }

    #[inline]
    fn peek16(&mut self) -> u32 {
        self.fill();
        (self.buf >> 48) as u32
    }

    #[inline]
    fn skip(&mut self, k: u32) {
        self.buf <<= k;
        self.n -= k;
    }

    #[inline]
    fn bits(&mut self, k: u32) -> u32 {
        if k == 0 {
            return 0;
        }
        self.fill();
        let v = (self.buf >> (64 - k)) as u32;
        self.skip(k);
        v
    }

    /// Bits read so far.
    fn pos(&self) -> usize {
        self.p * 8 - self.n as usize
    }

    /// Whether more was read than there is.
    fn past(&self) -> bool {
        self.pos() > self.d.len() * 8
    }
}

/// The bit writer: bits from the top of each 16-bit word, words little-endian.
#[derive(Default)]
pub struct BitW {
    pub out: Vec<u8>,
    acc: u32,
    n: u32,
}

impl BitW {
    /// A writer going on after `out`, at a word's start.
    pub fn new(out: Vec<u8>) -> BitW {
        BitW { out, acc: 0, n: 0 }
    }

    #[inline]
    pub fn bits(&mut self, k: u32, v: u32) {
        debug_assert!(k <= 16 && (k == 16 || v < 1 << k));
        self.acc = self.acc << k | v;
        self.n += k;
        if self.n >= 16 {
            self.n -= 16;
            let w = (self.acc >> self.n) as u16;
            self.out.extend_from_slice(&w.to_le_bytes());
            self.acc &= (1 << self.n) - 1;
        }
    }

    /// Up to 32 bits.
    #[inline]
    pub fn bits_long(&mut self, k: u32, v: u32) {
        if k > 16 {
            self.bits(k - 16, v >> 16);
            self.bits(16, v & 0xffff);
        } else {
            self.bits(k, v);
        }
    }

    /// Bits written into the current word.
    pub fn partial(&self) -> u32 {
        self.n
    }

    /// Pads to the next word with `v` (of the bits that need).
    fn align(&mut self, v: u16) -> Option<()> {
        if self.n > 0 {
            let k = 16 - self.n;
            ((v as u32) < 1 << k).then_some(())?;
            self.bits(k, v as u32);
        } else {
            (v == 0).then_some(())?;
        }
        Some(())
    }
}

/// Bits of the codes a table decodes at once; longer codes are found by their lengths.
const FAST: u32 = 10;

/// A canonical Huffman code of code lengths: codes by symbol, and to decode, a table of the
/// codes of at most [`FAST`] bits by their first bits ((symbol, length), 0 for a longer code)
/// and, for the longer ones, the first code of each length and its symbols in order.
pub struct Huff {
    pub codes: Vec<u16>,
    table: Vec<u32>,
    first: [u32; MAX_LEN as usize + 1],
    count: [u32; MAX_LEN as usize + 1],
    offset: [u32; MAX_LEN as usize + 1],
    sorted: Vec<u16>,
}

impl Huff {
    /// None where the lengths oversubscribe the code.
    pub fn new(lens: &[u8], decode: bool) -> Option<Huff> {
        let mut count = [0u32; MAX_LEN as usize + 1];
        for &l in lens {
            *count.get_mut(l as usize)? += 1;
        }
        count[0] = 0;
        let (mut first, mut offset) = ([0u32; MAX_LEN as usize + 1], [0u32; MAX_LEN as usize + 1]);
        let (mut code, mut at) = (0u32, 0u32);
        for l in 1..=MAX_LEN as usize {
            first[l] = code;
            offset[l] = at;
            if code + count[l] > 1 << l {
                return None;
            }
            at += count[l];
            code = (code + count[l]) << 1;
        }
        let mut sorted = vec![0u16; at as usize];
        let mut codes = vec![0u16; lens.len()];
        let mut next = offset;
        for (s, &l) in lens.iter().enumerate() {
            if l > 0 {
                let l = l as usize;
                let i = next[l];
                next[l] += 1;
                sorted[i as usize] = s as u16;
                codes[s] = (first[l] + i - offset[l]) as u16;
            }
        }
        let mut table = Vec::new();
        if decode {
            table = vec![0u32; 1 << FAST];
            for (s, &l) in lens.iter().enumerate() {
                if l > 0 && l as u32 <= FAST {
                    let start = (codes[s] as usize) << (FAST - l as u32);
                    table[start..start + (1 << (FAST - l as u32))].fill((s as u32) << 8 | l as u32);
                }
            }
        }
        Some(Huff { codes, table, first, count, offset, sorted })
    }

    /// The next symbol, or None for bits no code starts with.
    #[inline]
    fn decode(&self, r: &mut BitR) -> Option<usize> {
        let v = r.peek16();
        let e = self.table[(v >> (16 - FAST)) as usize];
        if e != 0 {
            r.skip(e & 0xff);
            return Some((e >> 8) as usize);
        }
        for l in FAST as usize + 1..=MAX_LEN as usize {
            let code = v >> (16 - l);
            if code.wrapping_sub(self.first[l]) < self.count[l] {
                r.skip(l as u32);
                return Some(self.sorted[(self.offset[l] + code - self.first[l]) as usize] as usize);
            }
        }
        None
    }
}

/// The code lengths `lens` after `code`, written against their values before.
fn read_tree(r: &mut BitR, lens: &mut [u8]) -> Option<TreeCode> {
    let mut t = TreeCode::default();
    for p in t.pre.iter_mut() {
        *p = r.bits(4) as u8;
    }
    let h = Huff::new(&t.pre, true)?;
    let mut i = 0;
    while i < lens.len() {
        let sym = h.decode(r)? as u8;
        let mut s = PreSym { sym, extra: 0, then: 0 };
        let (n, v) = match sym {
            17 => {
                s.extra = r.bits(4) as u8;
                (4 + s.extra as usize, Some(0))
            }
            18 => {
                s.extra = r.bits(5) as u8;
                (20 + s.extra as usize, Some(0))
            }
            19 => {
                s.extra = r.bits(1) as u8;
                s.then = h.decode(r)? as u8;
                if s.then > 16 {
                    return None;
                }
                (4 + s.extra as usize, Some((lens[i] + 17 - s.then) % 17))
            }
            _ => (1, None),
        };
        // (a run past the end is not one the decoder would take)
        if i + n > lens.len() {
            return None;
        }
        match v {
            Some(v) => lens[i..i + n].fill(v),
            None => lens[i] = (lens[i] + 17 - sym) % 17,
        }
        i += n;
        t.syms.push(s);
    }
    Some(t)
}

fn write_tree(w: &mut BitW, t: &TreeCode) -> Option<()> {
    t.pre.iter().for_each(|&l| w.bits(4, l as u32));
    let h = Huff::new(&t.pre, false)?;
    let put = |w: &mut BitW, s: u8| -> Option<()> {
        let l = *t.pre.get(s as usize).filter(|&&l| l > 0)?;
        w.bits(l as u32, h.codes[s as usize] as u32);
        Some(())
    };
    for s in &t.syms {
        put(w, s.sym)?;
        match s.sym {
            17 => w.bits(4, s.extra as u32),
            18 => w.bits(5, s.extra as u32),
            19 => {
                w.bits(1, s.extra as u32);
                put(w, s.then)?;
            }
            _ => {}
        }
    }
    Some(())
}

/// The code lengths `t` writes, against `lens` before.
pub fn apply_tree(t: &TreeCode, lens: &mut [u8]) -> Option<()> {
    let mut i = 0;
    for s in &t.syms {
        let (n, v) = match s.sym {
            17 => (4 + s.extra as usize, Some(0)),
            18 => (20 + s.extra as usize, Some(0)),
            19 => (4 + s.extra as usize, Some((*lens.get(i)? + 17 - s.then) % 17)),
            sym => (1, Some((*lens.get(i)? + 17 - sym) % 17)),
        };
        lens.get_mut(i..i + n)?.fill(v?);
        i += n;
    }
    (i == lens.len()).then_some(())
}

/// The tree lengths a block's trees give, after those before.
pub struct Lens {
    /// The main tree's lengths (as many as the variant's [`Variant::main`]) and the length tree's.
    pub main: Vec<u8>,
    pub len: [u8; LEN],
}

impl Default for Lens {
    fn default() -> Lens {
        Lens::new(WIM)
    }
}

impl Lens {
    pub fn new(v: Variant) -> Lens {
        Lens { main: vec![0; v.main()], len: [0; LEN] }
    }

    pub fn apply(&mut self, b: &Block) -> Option<()> {
        apply_tree(&b.trees[0], &mut self.main[..256])?;
        apply_tree(&b.trees[1], &mut self.main[256..])?;
        apply_tree(&b.trees[2], &mut self.len)
    }
}

/// The repeat offsets, as a chunk starts.
pub const REPS: [u32; 3] = [1, 1, 1];

/// Reads chunk `src` of `n` bytes of data: the data (as translated by e8, see [`e8_decode`])
/// and the parse. None where it is not a chunk this reads whole.
pub fn read(src: &[u8], n: usize) -> Option<(Vec<u8>, Parse)> {
    read_as(src, n, WIM)
}

/// Reads stream `src` of `v` of `n` bytes of data: the data (as translated by e8: see
/// [`e8_decode`] and [`e8_decode_frames`]) and the parse. None where it is not a stream this
/// reads whole.
pub fn read_as(src: &[u8], n: usize, v: Variant) -> Option<(Vec<u8>, Parse)> {
    read_impl(src, n, v, &mut Vec::new())
}

/// Where in stream `src` of `v` of `n` bytes of data each frame but the last ends: the bytes of
/// the frames up to it (as a CAB file's data blocks hold them).
pub fn frame_ends(src: &[u8], n: usize, v: Variant) -> Option<Vec<usize>> {
    let mut ends = Vec::new();
    read_impl(src, n, v, &mut ends)?;
    Some(ends)
}

fn read_impl(src: &[u8], n: usize, v: Variant, ends: &mut Vec<usize>) -> Option<(Vec<u8>, Parse)> {
    let mut r = BitR::new(src, 0);
    let mut data = Vec::with_capacity(n);
    let mut parse = Parse::default();
    let mut lens = Lens::new(v);
    let mut reps = REPS;
    let mut end_bits = 0;
    // (the end of the frame, where the stream is aligned in a block of tokens)
    let mut frame_end = if v.cab { FRAME.min(n) } else { usize::MAX };
    if v.cab && r.bits(1) == 1 {
        let hi = r.bits(16);
        parse.e8 = Some(hi << 16 | r.bits(16));
    }
    while data.len() < n {
        let mut b = Block { kind: r.bits(3) as u8, ..Block::default() };
        if v.cab {
            let hi = r.bits(16) as usize;
            b.size = hi << 8 | r.bits(8) as usize;
        } else {
            b.default_size = r.bits(1) == 1;
            b.size = if b.default_size { WINDOW } else { r.bits(16) as usize };
        }
        if b.size == 0 {
            return None;
        }
        // (the last block may say more than is left: the stream ends with its data)
        let end = (data.len() + b.size).min(n);
        match b.kind {
            VERBATIM | ALIGNED_BLOCK => {
                if b.kind == ALIGNED_BLOCK {
                    b.aligned.iter_mut().for_each(|a| *a = r.bits(3) as u8);
                }
                let aligned = Huff::new(&b.aligned, b.kind == ALIGNED_BLOCK)?;
                b.trees[0] = read_tree(&mut r, &mut lens.main[..256])?;
                b.trees[1] = read_tree(&mut r, &mut lens.main[256..])?;
                b.trees[2] = read_tree(&mut r, &mut lens.len)?;
                let (main, len_tree) = (Huff::new(&lens.main, true)?, Huff::new(&lens.len, true)?);
                let first = parse.toks.len();
                while data.len() < end {
                    let s = main.decode(&mut r)?;
                    if s < 256 {
                        data.push(s as u8);
                        parse.toks.push(Tok::Lit);
                    } else {
                        let (header, slot) = ((s - 256) & 7, (s - 256) >> 3);
                        let mut len = header + MIN_MATCH;
                        if header == 7 {
                            len += len_tree.decode(&mut r)?;
                        }
                        let (off, rep) = match slot {
                            0..3 => {
                                reps.swap(0, slot);
                                (reps[0], Some(slot as u8))
                            }
                            _ => {
                                let e = EXTRA[slot] as u32;
                                let f = if b.kind == ALIGNED_BLOCK && e >= 3 {
                                    BASE[slot] + (r.bits(e - 3) << 3) + aligned.decode(&mut r)? as u32
                                } else {
                                    BASE[slot] + r.bits(e)
                                };
                                reps = [f - 2, reps[0], reps[1]];
                                (f - 2, None)
                            }
                        };
                        let at = data.len();
                        if off == 0 || off as usize > at.min(v.max_off()) || at + len > end {
                            return None;
                        }
                        for i in at..at + len {
                            data.push(data[i - off as usize]);
                        }
                        parse.toks.push(Tok::Match { len: len as u16, off, rep });
                    }
                    if data.len() >= frame_end && frame_end < n {
                        let k = (16 - r.pos() % 16) % 16;
                        parse.frame_pad.push(r.bits(k as u32) as u16);
                        ends.push(r.pos() / 8);
                        frame_end = (frame_end + FRAME).min(n);
                    }
                }
                b.toks = parse.toks.len() - first;
                if r.past() {
                    return None;
                }
                end_bits = r.pos();
            }
            UNCOMPRESSED => {
                let pad = match r.pos() % 16 {
                    0 => 16,
                    k => 16 - k as u32,
                };
                b.align_bits = r.bits(pad) as u16;
                // (where the bytes start: the words not read yet)
                let mut q = r.pos() / 8;
                for rep in b.reps.iter_mut() {
                    *rep = u32::from_le_bytes(src.get(q..q + 4)?.try_into().unwrap());
                    q += 4;
                }
                if v.cab && b.reps.iter().any(|&r| r == 0 || r as usize > v.max_off()) {
                    return None;
                }
                reps = b.reps;
                let k = end - data.len();
                let (from, at) = (q, data.len());
                data.extend_from_slice(src.get(q..q + k)?);
                q += k;
                if b.size % 2 == 1 {
                    b.pad_byte = src.get(q).copied();
                    q += b.pad_byte.is_some() as usize;
                }
                r = BitR::new(src, q);
                end_bits = q * 8;
                while frame_end <= data.len() && frame_end < n {
                    ends.push(from + frame_end - at);
                    frame_end = (frame_end + FRAME).min(n);
                }
            }
            _ => return None,
        }
        parse.blocks.push(b);
    }
    // the bits padding the last word, and bytes after it
    let k = end_bits % 16;
    if k != 0 {
        let mut r = BitR::new(src, end_bits / 16 * 2);
        r.bits(k as u32);
        parse.tail_bits = r.bits(16 - k as u32) as u16;
    }
    let used = (end_bits.div_ceil(16) * 2).min(src.len());
    parse.trailing = src.get(used..)?.to_vec();
    Some((data, parse))
}

/// Writes chunk `parse` of data `data` (as translated by e8). None where the parse does not
/// hold (a code it uses has no length, a match out of the window or of its block).
pub fn write(data: &[u8], parse: &Parse) -> Option<Vec<u8>> {
    write_as(data, parse, WIM)
}

/// Writes stream `parse` of `v` of data `data` (as translated by e8).
pub fn write_as(data: &[u8], parse: &Parse, v: Variant) -> Option<Vec<u8>> {
    let mut w = BitW { out: Vec::with_capacity(data.len() / 2), ..BitW::default() };
    write_onto_as(data, parse, v, &mut w)?;
    Some(w.out)
}

/// [`write`] onto `w`, whose output is the chunk's alone.
pub fn write_onto(data: &[u8], parse: &Parse, w: &mut BitW) -> Option<()> {
    write_onto_as(data, parse, WIM, w)
}

/// [`write_as`] onto `w`, at a word's start.
pub fn write_onto_as(data: &[u8], parse: &Parse, v: Variant, w: &mut BitW) -> Option<()> {
    let mut lens = Lens::new(v);
    let mut reps = REPS;
    let (mut at, mut t, mut f) = (0, 0, 0);
    let n = data.len();
    let mut frame_end = if v.cab { FRAME.min(n) } else { usize::MAX };
    if v.cab {
        w.bits(1, parse.e8.is_some() as u32);
        if let Some(e8) = parse.e8 {
            w.bits(16, e8 >> 16);
            w.bits(16, e8 & 0xffff);
        }
    } else if parse.e8.is_some() || !parse.frame_pad.is_empty() {
        return None;
    }
    for b in &parse.blocks {
        w.bits(3, b.kind as u32);
        if v.cab {
            (b.size < 1 << 24 && !b.default_size).then_some(())?;
            w.bits(16, (b.size >> 8) as u32);
            w.bits(8, (b.size & 0xff) as u32);
        } else {
            w.bits(1, b.default_size as u32);
            if b.default_size {
                (b.size == WINDOW).then_some(())?;
            } else {
                w.bits(16, u16::try_from(b.size).ok()? as u32);
            }
        }
        let end = (at + b.size).min(n);
        match b.kind {
            VERBATIM | ALIGNED_BLOCK => {
                if b.kind == ALIGNED_BLOCK {
                    b.aligned.iter().for_each(|&a| w.bits(3, a as u32));
                }
                write_tree(w, &b.trees[0])?;
                write_tree(w, &b.trees[1])?;
                write_tree(w, &b.trees[2])?;
                lens.apply(b)?;
                let (main, len_tree) = (Huff::new(&lens.main, false)?, Huff::new(&lens.len, false)?);
                let aligned = Huff::new(&b.aligned, false)?;
                let code = |w: &mut BitW, h: &Huff, l: &[u8], s: usize| -> Option<()> {
                    let l = *l.get(s).filter(|&&l| l > 0)?;
                    w.bits(l as u32, h.codes[s] as u32);
                    Some(())
                };
                for tok in parse.toks.get(t..t + b.toks)? {
                    match *tok {
                        Tok::Lit => {
                            code(w, &main, &lens.main, *data.get(at)? as usize)?;
                            at += 1;
                        }
                        Tok::Match { len, off, rep } => {
                            let len = len as usize;
                            (MIN_MATCH..=MAX_MATCH).contains(&len).then_some(())?;
                            let header = (len - MIN_MATCH).min(7);
                            let (slot, fo) = match rep {
                                Some(k) if k < 3 => {
                                    reps.swap(0, k as usize);
                                    (reps[0] == off).then_some(())?;
                                    (k as usize, 0)
                                }
                                Some(_) => return None,
                                None => {
                                    let fo = off + 2;
                                    reps = [off, reps[0], reps[1]];
                                    (slot(fo), fo)
                                }
                            };
                            let ok = off as usize >= 1 && off as usize <= at.min(v.max_off()) && at + len <= end;
                            ok.then_some(())?;
                            code(w, &main, &lens.main, 256 + slot * 8 + header)?;
                            if header == 7 {
                                code(w, &len_tree, &lens.len, len - MIN_MATCH - 7)?;
                            }
                            if slot >= 3 {
                                let (e, x) = (EXTRA[slot] as u32, fo - BASE[slot]);
                                if b.kind == ALIGNED_BLOCK && e >= 3 {
                                    w.bits_long(e - 3, x >> 3);
                                    code(w, &aligned, &b.aligned, (x & 7) as usize)?;
                                } else {
                                    w.bits_long(e, x);
                                }
                            }
                            at += len;
                        }
                    }
                    if at >= frame_end && frame_end < n {
                        w.align(*parse.frame_pad.get(f)?)?;
                        f += 1;
                        frame_end = (frame_end + FRAME).min(n);
                    }
                }
                t += b.toks;
                (at == end).then_some(())?;
            }
            UNCOMPRESSED => {
                let pad = 16 - w.partial();
                ((b.align_bits as u32) < 1 << pad).then_some(())?;
                w.bits(pad, b.align_bits as u32);
                b.reps.iter().for_each(|r| w.out.extend_from_slice(&r.to_le_bytes()));
                reps = b.reps;
                w.out.extend_from_slice(&data[at..end]);
                (end - at == b.size || end == n).then_some(())?;
                if b.size % 2 == 1 {
                    w.out.extend(b.pad_byte);
                }
                at = end;
                while frame_end <= at && frame_end < n {
                    frame_end = (frame_end + FRAME).min(n);
                }
            }
            _ => return None,
        }
    }
    (at == n && t == parse.toks.len() && f == parse.frame_pad.len()).then_some(())?;
    if w.partial() > 0 {
        let k = 16 - w.partial();
        w.bits(k, parse.tail_bits as u32 & ((1 << k) - 1));
    }
    w.out.extend_from_slice(&parse.trailing);
    Some(())
}

/// Undoes e8 translation: the data a chunk decodes to into the file's.
pub fn e8_decode(d: &mut [u8]) {
    e8_decode_at(d, 0, E8_SIZE);
}

/// e8 translation, the inverse of [`e8_decode`]: the file's data into what a chunk codes.
pub fn e8_encode(d: &mut [u8]) {
    e8_encode_at(d, 0, E8_SIZE);
}

/// Frames of e8 translation: the first 2^30 bytes of a stream, in frames of more than 10 bytes.
fn frames(len: usize) -> impl Iterator<Item = std::ops::Range<usize>> {
    (0..len.min(1 << 30)).step_by(FRAME).map(move |s| s..(s + FRAME).min(len)).filter(|f| f.len() > 10)
}

/// Undoes the e8 translation of size `size` of a CAB stream's data `d`, frame by frame.
pub fn e8_decode_frames(d: &mut [u8], size: u32) {
    for f in frames(d.len()) {
        e8_decode_at(&mut d[f.clone()], f.start, size as i32);
    }
}

/// e8 translation of size `size` of a CAB stream's data `d`, frame by frame.
pub fn e8_encode_frames(d: &mut [u8], size: u32) {
    for f in frames(d.len()) {
        e8_encode_at(&mut d[f.clone()], f.start, size as i32);
    }
}

/// Undoes e8 translation of size `size` in `d`, which starts at `start` of its stream.
pub fn e8_decode_at(d: &mut [u8], start: usize, size: i32) {
    if d.len() <= 10 {
        return;
    }
    let mut i = 0;
    while i < d.len() - 10 {
        if d[i] == 0xe8 {
            let at = (start + i) as i32;
            let abs = i32::from_le_bytes(d[i + 1..i + 5].try_into().unwrap());
            if abs >= -at && abs < size {
                let rel = if abs >= 0 { abs - at } else { abs + size };
                d[i + 1..i + 5].copy_from_slice(&rel.to_le_bytes());
            }
            i += 5;
        } else {
            i += 1;
        }
    }
}

/// e8 translation, the inverse of [`e8_decode_at`].
pub fn e8_encode_at(d: &mut [u8], start: usize, size: i32) {
    if d.len() <= 10 {
        return;
    }
    let mut i = 0;
    while i < d.len() - 10 {
        if d[i] == 0xe8 {
            let at = (start + i) as i32;
            let rel = i32::from_le_bytes(d[i + 1..i + 5].try_into().unwrap());
            if rel >= -at && rel < size {
                let abs = if rel < size - at { rel + at } else { rel - size };
                d[i + 1..i + 5].copy_from_slice(&abs.to_le_bytes());
            }
            i += 5;
        } else {
            i += 1;
        }
    }
}
