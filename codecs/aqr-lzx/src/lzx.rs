//! LZX chunks as WIM files hold them (`FORMAT.md` §3): each chunk of at most 32 KiB compressed
//! on its own, with a window of 32 KiB, its data translated by e8 with a size of 12000000. A
//! chunk is read into its [`Parse`], everything its bits say, and written back from it.
//!
//! The bitstream is 16-bit little-endian words, read from their top bit. A block is a type (3
//! bits), a size (a bit telling 32768, else 16 bits), for aligned blocks the aligned tree,
//! then the main and length trees (coded against the block before's by a pretree each) and the
//! tokens; or, for an uncompressed block, its bytes after the stream is aligned to a word.

/// Bytes of a chunk, and of the window.
pub const WINDOW: usize = 32768;
/// Position slots of a window of 32 KiB.
pub const SLOTS: usize = 30;
pub const MAIN: usize = 256 + 8 * SLOTS;
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

/// Extra bits of each position slot.
pub const EXTRA: [u8; SLOTS] = {
    let mut e = [0; SLOTS];
    let mut s = 4;
    while s < SLOTS {
        e[s] = ((s - 2) / 2) as u8;
        s += 1;
    }
    e
};

/// The smallest formatted offset of each position slot.
pub const BASE: [u32; SLOTS] = {
    let mut b = [0; SLOTS];
    let mut s = 1;
    while s < SLOTS {
        b[s] = b[s - 1] + (1 << EXTRA[s - 1]);
        s += 1;
    }
    b
};

/// The position slot of formatted offset `f` (at least 3).
pub fn slot(f: u32) -> usize {
    BASE.partition_point(|&b| b <= f) - 1
}

/// A token: a literal (the next byte of the data), or a match of `len` bytes `off` back, coded
/// as repeat `rep` where given (the offset in that slot of the three last ones).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tok {
    Lit,
    Match { len: u16, off: u16, rep: Option<u8> },
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

/// A chunk's parse: its blocks and tokens, then the bits padding its last word and whatever
/// bytes follow.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Parse {
    pub blocks: Vec<Block>,
    pub toks: Vec<Tok>,
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

    /// Bits written into the current word.
    pub fn partial(&self) -> u32 {
        self.n
    }
}

/// A canonical Huffman code of code lengths: codes by symbol, and a table of 2^16 entries of
/// (symbol, length) to decode.
pub struct Huff {
    pub codes: Vec<u16>,
    table: Vec<u32>,
}

impl Huff {
    /// None where the lengths oversubscribe the code.
    pub fn new(lens: &[u8], decode: bool) -> Option<Huff> {
        let mut codes = vec![0u16; lens.len()];
        let mut table = if decode { vec![0u32; 1 << 16] } else { Vec::new() };
        let mut next: u32 = 0;
        for len in 1..=MAX_LEN {
            for (s, &l) in lens.iter().enumerate() {
                if l != len {
                    continue;
                }
                if next >= 1 << len {
                    return None;
                }
                codes[s] = next as u16;
                if decode {
                    let span = 1u32 << (16 - len);
                    let start = next << (16 - len);
                    table[start as usize..(start + span) as usize].fill((s as u32) << 8 | len as u32);
                }
                next += 1;
            }
            next <<= 1;
        }
        Some(Huff { codes, table })
    }

    /// The next symbol, or None for bits no code starts with.
    #[inline]
    fn decode(&self, r: &mut BitR) -> Option<usize> {
        let e = self.table[r.peek16() as usize];
        if e == 0 {
            return None;
        }
        r.skip(e & 0xff);
        Some((e >> 8) as usize)
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
    pub main: [u8; MAIN],
    pub len: [u8; LEN],
}

impl Default for Lens {
    fn default() -> Lens {
        Lens { main: [0; MAIN], len: [0; LEN] }
    }
}

impl Lens {
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
    let mut r = BitR::new(src, 0);
    let mut data = Vec::with_capacity(n);
    let mut parse = Parse::default();
    let mut lens = Lens::default();
    let mut reps = REPS;
    let mut end_bits;
    loop {
        let mut b = Block { kind: r.bits(3) as u8, default_size: r.bits(1) == 1, ..Block::default() };
        b.size = if b.default_size { WINDOW } else { r.bits(16) as usize };
        if b.size == 0 {
            return None;
        }
        // (the last block may say more than is left: the chunk ends with its data)
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
                        continue;
                    }
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
                    if off == 0 || off as usize > at || at + len > end {
                        return None;
                    }
                    for i in at..at + len {
                        data.push(data[i - off as usize]);
                    }
                    parse.toks.push(Tok::Match { len: len as u16, off: off as u16, rep });
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
                reps = b.reps;
                let k = end - data.len();
                data.extend_from_slice(src.get(q..q + k)?);
                q += k;
                if b.size % 2 == 1 {
                    b.pad_byte = src.get(q).copied();
                    q += b.pad_byte.is_some() as usize;
                }
                r = BitR::new(src, q);
                end_bits = q * 8;
            }
            _ => return None,
        }
        parse.blocks.push(b);
        if data.len() == n {
            break;
        }
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
    let mut w = BitW { out: Vec::with_capacity(data.len() / 2), ..BitW::default() };
    write_onto(data, parse, &mut w)?;
    Some(w.out)
}

/// [`write`] onto `w`, whose output is the chunk's alone.
pub fn write_onto(data: &[u8], parse: &Parse, w: &mut BitW) -> Option<()> {
    let start = w.out.len();
    let mut lens = Lens::default();
    let mut reps = REPS;
    let (mut at, mut t) = (0, 0);
    for b in &parse.blocks {
        w.bits(3, b.kind as u32);
        w.bits(1, b.default_size as u32);
        if b.default_size {
            (b.size == WINDOW).then_some(())?;
        } else {
            w.bits(16, u16::try_from(b.size).ok()? as u32);
        }
        let end = (at + b.size).min(data.len());
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
                            let (slot, f) = match rep {
                                Some(k) if k < 3 => {
                                    reps.swap(0, k as usize);
                                    (reps[0] == off as u32).then_some(())?;
                                    (k as usize, 0)
                                }
                                Some(_) => return None,
                                None => {
                                    let f = off as u32 + 2;
                                    reps = [off as u32, reps[0], reps[1]];
                                    (slot(f), f)
                                }
                            };
                            (off as usize >= 1 && off as usize <= at && at + len <= end).then_some(())?;
                            code(w, &main, &lens.main, 256 + slot * 8 + header)?;
                            if header == 7 {
                                code(w, &len_tree, &lens.len, len - MIN_MATCH - 7)?;
                            }
                            if slot >= 3 {
                                let (e, v) = (EXTRA[slot] as u32, f - BASE[slot]);
                                if b.kind == ALIGNED_BLOCK && e >= 3 {
                                    w.bits(e - 3, v >> 3);
                                    code(w, &aligned, &b.aligned, (v & 7) as usize)?;
                                } else {
                                    w.bits(e, v);
                                }
                            }
                            at += len;
                        }
                    }
                }
                t += b.toks;
                (at == end).then_some(())?;
            }
            UNCOMPRESSED => {
                let pad = 16 - w.partial();
                w.bits(pad, b.align_bits as u32);
                b.reps.iter().for_each(|r| w.out.extend_from_slice(&r.to_le_bytes()));
                reps = b.reps;
                w.out.extend_from_slice(&data[at..end]);
                (end - at == b.size || end == data.len()).then_some(())?;
                if b.size % 2 == 1 {
                    w.out.extend(b.pad_byte);
                }
                at = end;
            }
            _ => return None,
        }
    }
    (at == data.len() && t == parse.toks.len()).then_some(())?;
    if w.partial() > 0 {
        let k = 16 - w.partial();
        w.bits(k, parse.tail_bits as u32 & ((1 << k) - 1));
    }
    let _ = start;
    w.out.extend_from_slice(&parse.trailing);
    Some(())
}

/// Undoes e8 translation: the data a chunk decodes to into the file's.
pub fn e8_decode(d: &mut [u8]) {
    if d.len() <= 10 {
        return;
    }
    let mut i = 0;
    while i < d.len() - 10 {
        if d[i] == 0xe8 {
            let abs = i32::from_le_bytes(d[i + 1..i + 5].try_into().unwrap());
            if abs >= -(i as i32) && abs < E8_SIZE {
                let rel = if abs >= 0 { abs - i as i32 } else { abs + E8_SIZE };
                d[i + 1..i + 5].copy_from_slice(&rel.to_le_bytes());
            }
            i += 5;
        } else {
            i += 1;
        }
    }
}

/// e8 translation, the inverse of [`e8_decode`]: the file's data into what a chunk codes.
pub fn e8_encode(d: &mut [u8]) {
    if d.len() <= 10 {
        return;
    }
    let mut i = 0;
    while i < d.len() - 10 {
        if d[i] == 0xe8 {
            let rel = i32::from_le_bytes(d[i + 1..i + 5].try_into().unwrap());
            if rel >= -(i as i32) && rel < E8_SIZE {
                let abs = if rel < E8_SIZE - i as i32 { rel + i as i32 } else { rel - E8_SIZE };
                d[i + 1..i + 5].copy_from_slice(&abs.to_le_bytes());
            }
            i += 5;
        } else {
            i += 1;
        }
    }
}
