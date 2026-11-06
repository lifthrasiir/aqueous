//! A fast parser of raw DEFLATE streams into the decompressed data, one flat list of tokens
//! and the blocks' structure. It gives exactly what [`crate::deflate::parse`] gives (with
//! the tokens moved out of the blocks), several times faster: a 64-bit bit buffer and
//! two-level decoding tables (small enough to stay in L1). Statistics the rest needs of the
//! tokens are gathered on the way, sparing passes over them.
//!
//! [`parse_into`] hands the content to a [`Sink`] instead of collecting it, for callers
//! that process the tokens as they are decoded.

use crate::deflate::{Block, Body, DynHeader, Stream, Token};
use crate::huffman::{self, CL_ORDER, DIST_BASE, DIST_EXTRA, LEN_BASE, LEN_EXTRA, RleOp};
use crate::matchfind::MAX_MATCH;
use crate::{Error, Result};

/// A parsed stream with its tokens in one list.
pub struct Flat {
    /// The blocks, without their tokens.
    pub stream: Stream,
    pub data: Data,
    pub tokens: Vec<Token>,
    /// Per block, its first token (or where it would be, for stored blocks), then the end.
    pub starts: Vec<usize>,
    /// Stored blocks as (token index before which they come, bytes).
    pub skips: Vec<(usize, usize)>,
    pub stats: Stats,
    /// Where `tokens` holds only some of the tokens ([`parse_segments_head`]), the runs of them
    /// it holds; else empty (it holds all).
    pub chunks: Vec<Chunk>,
}

/// The data of a [`Flat`]: with the feature `encode`, bytes of aqr-formula's store where the
/// parse was told to write it there ([`parse_segments_head`]'s `store`), else of the heap.
#[cfg(feature = "encode")]
pub type Data = aqr_formula::Bytes;
#[cfg(not(feature = "encode"))]
pub type Data = Vec<u8>;

/// A run of tokens that [`parse_segments_head`] keeps: from the token of index `token` (in
/// the stream), at `pos` bytes of data, `count` tokens, at index `kept` of [`Flat::tokens`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Chunk {
    pub token: usize,
    pub kept: usize,
    pub pos: usize,
    pub count: usize,
}

/// Bytes of data that each run of [`parse_segments_head`] after the first covers: [`WARM`]
/// bytes, for a coder to fill its tables with, then 8 KiB.
pub const CHUNK: usize = WARM + (8 << 10);
/// Bytes at the start of each run after the first that only fill a coder's tables.
pub const WARM: usize = 8 << 10;

/// Where the run after one starting at `pos` starts: 64 KiB on, or an eighth of `pos` on, so
/// that the runs sample a stream of any length (its length not known as it is parsed) with
/// about 8 per doubling of it.
pub fn next_chunk(pos: usize) -> usize {
    pos + (pos / 8).max(64 << 10)
}

/// Per block, its symbol frequencies: 288 literal/length (end of block counted once), 32
/// distance (zeros for stored blocks).
pub type Freqs = ([u32; 288], [u32; 32]);

#[derive(Default)]
pub struct Stats {
    pub freqs: Vec<Freqs>,
    /// The farthest distance.
    pub max_dist: u16,
    /// Whether matches of length 3 go beyond 4096 (bit 0) and 8192 (bit 1), and of length
    /// up to 4 beyond 4096 (bit 2).
    pub far: u8,
    /// Token indices where segments of about `segment` bytes start (0 first), cut before
    /// tokens that no stored block comes before; and the positions of those tokens.
    pub cuts: Vec<usize>,
    pub cut_pos: Vec<usize>,
    segment: usize,
    /// Where the next token is looked at: a cut may come at it, or the data is beyond `max`
    /// past it.
    next: usize,
    /// No cut before this token index (where stored blocks were).
    no_cut: usize,
    /// Bytes of data allowed.
    max: usize,
}

impl Stats {
    #[inline(always)]
    fn token_at(&mut self, k: usize, p: usize) {
        if p >= self.next && k > 0 && k != self.no_cut {
            self.cuts.push(k);
            self.cut_pos.push(p);
            self.next = p
                .saturating_add(self.segment)
                .min(self.max.saturating_add(1));
        }
    }
}

#[derive(Clone, Copy)]
struct Bits<'a> {
    d: &'a [u8],
    /// Next byte to load.
    pos: usize,
    buf: u64,
    n: u32,
}

impl<'a> Bits<'a> {
    #[inline(always)]
    fn refill(&mut self) {
        if self.n > 56 {
            return;
        }
        if let Some(w) = self.d.get(self.pos..self.pos + 8) {
            let w = u64::from_le_bytes(w.try_into().unwrap());
            self.buf |= w << self.n;
            let k = (63 - self.n) / 8;
            self.pos += k as usize;
            self.n += k * 8;
        } else {
            while self.n <= 56 {
                // Past the end: zeros, caught by `check`.
                let b = self.d.get(self.pos).copied().unwrap_or(0);
                self.buf |= (b as u64) << self.n;
                self.pos += 1;
                self.n += 8;
            }
        }
    }

    #[inline(always)]
    fn bits(&mut self, k: u32) -> u32 {
        debug_assert!(k <= 32 && k <= self.n);
        let v = (self.buf & ((1u64 << k) - 1)) as u32;
        self.buf >>= k;
        self.n -= k;
        v
    }

    #[inline(always)]
    fn get(&mut self, k: u32) -> u32 {
        self.refill();
        self.bits(k)
    }

    /// Bits consumed so far.
    fn bit_pos(&self) -> usize {
        self.pos * 8 - self.n as usize
    }

    /// Fails if more bits were consumed than the input has.
    fn check(&self) -> Result<()> {
        if self.bit_pos() > self.d.len() * 8 {
            Err(Error::Truncated)
        } else {
            Ok(())
        }
    }
}

/// Entry flag: the entry points to a subtable (offset | subtable bits << 16).
const SUB: u32 = 1 << 31;
/// Primary table bits of the literal/length, distance and code-length codes.
const LIT_BITS: u32 = 11;
const DIST_BITS: u32 = 8;
const CL_BITS: u32 = 7;

/// A two-level decoding table: a primary table indexed by `bits` bits, entry = symbol |
/// code length << 16 (0: no code), and for longer codes subtables indexed by the bits that
/// follow (entry lengths then count only those bits).
struct Table {
    t: Vec<u32>,
    bits: u32,
    mask: u64,
}

impl Table {
    /// A table to be built.
    fn empty(bits: u32) -> Table {
        Table {
            t: vec![],
            bits,
            mask: (1 << bits) - 1,
        }
    }

    fn new(lens: &[u8], bits: u32) -> Result<Table> {
        let mut t = Table::empty(bits);
        t.build(lens)?;
        Ok(t)
    }

    /// Rebuilds the table for `lens`, reusing its memory.
    fn build(&mut self, lens: &[u8]) -> Result<()> {
        huffman::check_lens(lens)?;
        let mut count = [0u32; 16];
        for &l in lens {
            count[l as usize] += 1;
        }
        count[0] = 0;
        let mut next = [0u32; 16];
        let mut code = 0;
        for l in 1..16 {
            code = (code + count[l - 1]) << 1;
            next[l] = code;
        }
        let p = self.bits;
        let psize = 1usize << p;
        // Subtable sizes: the longest code under each primary prefix (codes are read
        // LSB first, so the prefix is the low bits of the reversed code).
        let mut longest = [0u8; 1 << LIT_BITS];
        let mut first = next;
        let max = (1..16).rev().find(|&l| count[l] > 0).unwrap_or(0) as u32;
        for &l in if max > p { lens } else { &[] } {
            if l as u32 > p {
                let r = first[l as usize].reverse_bits() >> (32 - l as u32);
                first[l as usize] += 1;
                let i = r as usize & (psize - 1);
                longest[i] = longest[i].max(l);
            }
        }
        // Entries no code reaches read as no code (incomplete codes are legal).
        self.t.clear();
        self.t.resize(psize, 0);
        for (i, &l) in longest[..if max > p { psize } else { 0 }]
            .iter()
            .enumerate()
        {
            if l > 0 {
                let sb = l as u32 - p;
                self.t[i] = self.t.len() as u32 | sb << 16 | SUB;
                self.t.resize(self.t.len() + (1 << sb), 0);
            }
        }
        for (sym, &l) in lens.iter().enumerate() {
            if l == 0 {
                continue;
            }
            let l = l as u32;
            let r = next[l as usize].reverse_bits() >> (32 - l);
            next[l as usize] += 1;
            if l <= p {
                let e = sym as u32 | l << 16;
                let mut i = r as usize;
                while i < psize {
                    self.t[i] = e;
                    i += 1 << l;
                }
            } else {
                let pe = self.t[r as usize & (psize - 1)];
                let (off, sb) = ((pe & 0xffff) as usize, (pe >> 16) & 0xff);
                let e = sym as u32 | (l - p) << 16;
                let mut i = (r >> p) as usize;
                while i < 1 << sb {
                    self.t[off + i] = e;
                    i += 1 << (l - p);
                }
            }
        }
        Ok(())
    }

    /// Decodes a symbol (15 bits must be buffered).
    #[inline(always)]
    fn decode(&self, b: &mut Bits) -> Result<u32> {
        let mut e = self.t[(b.buf & self.mask) as usize];
        if e & SUB != 0 {
            b.bits(self.bits);
            let sb = (e >> 16) & 0xff;
            e = self.t[(e & 0xffff) as usize + (b.buf & ((1 << sb) - 1)) as usize];
        }
        let l = e >> 16;
        if l == 0 {
            return Err(Error::Invalid("undecodable Huffman code"));
        }
        b.bits(l);
        Ok(e & 0xffff)
    }
}

/// Receives the content of a stream from [`parse_into`], in order.
pub trait Sink {
    /// Called before every token: the next call is at most three literals or one match.
    fn reserve(&mut self) {}
    fn lit(&mut self, b: u8);
    /// A match; `1 <= dist <=` the bytes so far (checked by the parser). `alt258`: length
    /// 258 sent as symbol 284 with extra bits 31.
    fn mat(&mut self, len: u16, dist: u16, alt258: bool);
    /// The bytes of a stored block.
    fn stored(&mut self, bytes: &[u8]);
}

/// What [`parse_into`] gives: a [`Flat`] without the data and tokens.
pub struct Layout {
    pub stream: Stream,
    pub starts: Vec<usize>,
    pub skips: Vec<(usize, usize)>,
    pub stats: Stats,
    /// Bytes of data.
    pub len: usize,
    /// Bytes of the stream: where its trailing bytes start in `raw`.
    pub end: usize,
}

/// Collects the data and tokens (for [`parse_segments`]). The vectors are kept as raw
/// parts and grown by value, so that their lengths can stay in registers in the token loop
/// (a `&mut Vec` would pin them to memory).
struct Collect<const HEAD: bool> {
    data: (*mut u8, usize, usize),
    tokens: (*mut Token, usize, usize),
    /// With `HEAD`, the tokens are kept in runs only (the first of `first` bytes of data, the
    /// others of [`CHUNK`] bytes as [`next_chunk`] places them): whether they are now, at how
    /// many bytes of data that changes, and where the next run starts (both as `data` holds
    /// the data: from `base` on).
    keeping: bool,
    turn: usize,
    next: usize,
    /// Where the data goes to aqr-formula's store as it is made ([`Collect::new`]), what of it
    /// is written: out of the hot fields, as it is seldom used.
    #[cfg(feature = "encode")]
    out: Option<Box<Out>>,
    /// Tokens seen, and the runs kept.
    seen: usize,
    chunks: Vec<Chunk>,
}

// Room the collector keeps per token: a match and the overshoot of its copy.
const ROOM: usize = MAX_MATCH + 16;

/// Grows the vector with raw parts `v` to room for `extra` more elements.
#[inline(never)]
#[cold]
fn grow<T>(v: (*mut T, usize, usize), extra: usize) -> (*mut T, usize, usize) {
    // SAFETY: the parts come from a vector (`Collect::new` or an earlier `grow`) and the
    // first `v.1` elements are initialized.
    let mut w = std::mem::ManuallyDrop::new(unsafe { Vec::from_raw_parts(v.0, v.1, v.2) });
    w.reserve(extra.max(v.1 / 2));
    (w.as_mut_ptr(), w.len(), w.capacity())
}

impl<const HEAD: bool> Collect<HEAD> {
    /// Room for `data` bytes of data and `tokens` tokens, the first run of `first` bytes; the
    /// data written to aqr-formula's store as it is made where `store` (with the feature
    /// `encode`): then the room is for a window of it.
    fn new(data: usize, tokens: usize, first: usize, store: bool) -> Collect<HEAD> {
        let store = store && cfg!(feature = "encode");
        let data = if store { data.min(STORE_WINDOW) } else { data };
        #[cfg(feature = "encode")]
        let out = store.then(|| {
            Box::new(Out {
                writer: None,
                base: 0,
            })
        });
        let mut d = std::mem::ManuallyDrop::new(Vec::<u8>::with_capacity(data));
        let mut t = std::mem::ManuallyDrop::new(Vec::<Token>::with_capacity(tokens));
        Collect {
            data: (d.as_mut_ptr(), 0, d.capacity()),
            tokens: (t.as_mut_ptr(), 0, t.capacity()),
            keeping: true,
            turn: first,
            next: next_chunk(0).max(first),
            seen: 0,
            #[cfg(feature = "encode")]
            out,
            chunks: if HEAD {
                vec![Chunk {
                    token: 0,
                    kept: 0,
                    pos: 0,
                    count: 0,
                }]
            } else {
                Vec::new()
            },
        }
    }

    /// Whether the token starting now is kept.
    #[inline(always)]
    fn keep(&mut self) -> bool {
        if !HEAD {
            return true;
        }
        while self.data.1 >= self.turn {
            self.turn();
        }
        self.seen += 1;
        self.keeping
    }

    /// Ends the run being kept, or starts the next.
    #[cold]
    #[inline(never)]
    fn turn(&mut self) {
        if self.keeping {
            let c = self.chunks.last_mut().expect("a run");
            c.count = self.tokens.1 - c.kept;
            self.keeping = false;
            self.turn = self.next;
        } else {
            let base = self.base();
            let pos = base + self.data.1;
            self.chunks.push(Chunk {
                token: self.seen,
                kept: self.tokens.1,
                pos,
                count: 0,
            });
            self.keeping = true;
            self.turn = self.data.1 + CHUNK;
            self.next = next_chunk(pos) - base;
        }
    }

    /// Bytes of data before what `data` holds ([`Out`]).
    fn base(&self) -> usize {
        #[cfg(feature = "encode")]
        return self.out.as_ref().map_or(0, |o| o.base);
        #[cfg(not(feature = "encode"))]
        0
    }

    /// Room for `extra` more bytes of data: the data written out but the last [`HISTORY`]
    /// bytes where it goes out ([`Collect::new`]), else more of it. (Without the feature
    /// `encode`, `grow` alone, as it was.)
    #[cfg_attr(feature = "encode", cold, inline(never))]
    #[cfg_attr(not(feature = "encode"), inline(always))]
    fn room(&mut self, extra: usize) {
        #[cfg(feature = "encode")]
        if let Some(out) = &mut self.out
            && self.data.1 + extra > STORE_FROM
        {
            let w = out
                .writer
                .get_or_insert_with(|| aqr_formula::store::Writer::new(None));
            let cut = self.data.1.saturating_sub(HISTORY);
            // SAFETY: the first `data.1` bytes are made.
            let made = unsafe { std::slice::from_raw_parts(self.data.0, self.data.1) };
            w.write(&made[..cut]);
            // SAFETY: within the first `data.1` bytes.
            unsafe { std::ptr::copy(self.data.0.add(cut), self.data.0, self.data.1 - cut) };
            self.data.1 -= cut;
            out.base += cut;
            self.turn = self.turn.saturating_sub(cut);
            self.next = self.next.saturating_sub(cut);
            if self.data.2 - self.data.1 >= extra {
                return;
            }
        }
        self.data = grow(self.data, extra);
    }

    fn into_vecs(mut self) -> (Data, Vec<Token>, Vec<Chunk>) {
        if self.keeping
            && let Some(c) = self.chunks.last_mut()
        {
            c.count = self.tokens.1 - c.kept;
        }
        // SAFETY: as in `grow`.
        let (mut data, mut tokens) = unsafe {
            (
                Vec::from_raw_parts(self.data.0, self.data.1, self.data.2),
                Vec::from_raw_parts(self.tokens.0, self.tokens.1, self.tokens.2),
            )
        };
        #[cfg(feature = "encode")]
        if let Some(mut w) = self.out.take().and_then(|o| o.writer) {
            w.write(&data);
            tokens.shrink_to_fit();
            return (w.into_bytes(), tokens, self.chunks);
        }
        // (the room grown into given back: up to as much again as the data, held as long as it)
        data.shrink_to_fit();
        tokens.shrink_to_fit();
        (data.into(), tokens, self.chunks)
    }
}

/// The data of a [`Collect`] going out as it is made: the writer (once there is more than
/// [`STORE_FROM`] bytes of it), and how many bytes are before what the collector holds.
#[cfg(feature = "encode")]
struct Out {
    writer: Option<aqr_formula::store::Writer>,
    base: usize,
}

/// Bytes of data from which [`Collect`] writes it out as it is made, where told to.
const STORE_FROM: usize = 1 << 20;

/// Bytes of the window of data that [`Collect`] holds where the data is written out as it is
/// made: [`HISTORY`] bytes back, and a stored block's (65,535 at most) or a match's (with its
/// overshoot) beyond [`STORE_FROM`].
const STORE_WINDOW: usize = HISTORY + STORE_FROM + 65535 + ROOM;

impl<const HEAD: bool> Sink for Collect<HEAD> {
    #[inline(always)]
    fn reserve(&mut self) {
        if self.data.2 - self.data.1 < ROOM {
            self.room(ROOM);
        }
        if self.tokens.2 - self.tokens.1 < 3 {
            self.tokens = grow(self.tokens, 3);
        }
    }

    #[inline(always)]
    fn lit(&mut self, b: u8) {
        // SAFETY: `reserve` came before, and leaves room for three literals.
        let keep = self.keep();
        unsafe {
            *self.data.0.add(self.data.1) = b;
            if keep {
                self.tokens.0.add(self.tokens.1).write(Token::Lit(b));
            }
        }
        self.data.1 += 1;
        self.tokens.1 += keep as usize;
    }

    #[inline(always)]
    fn mat(&mut self, len: u16, dist: u16, alt258: bool) {
        // SAFETY: `1 <= dist <=` the bytes so far (checked by the parser), and `reserve`
        // left room for the match and its overshoot.
        let keep = self.keep();
        unsafe {
            copy_match(self.data.0, self.data.1, dist as usize, len as usize);
            if keep {
                self.tokens
                    .0
                    .add(self.tokens.1)
                    .write(Token::Match { len, dist, alt258 });
            }
        }
        self.data.1 += len as usize;
        self.tokens.1 += keep as usize;
    }

    fn stored(&mut self, bytes: &[u8]) {
        if self.data.2 - self.data.1 < bytes.len() + ROOM {
            self.room(bytes.len() + ROOM);
        }
        // SAFETY: room made just above.
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), self.data.0.add(self.data.1), bytes.len())
        };
        self.data.1 += bytes.len();
    }
}

/// Parses `raw` (see the module documentation).
pub fn parse(raw: &[u8]) -> Result<Flat> {
    parse_segments(raw, usize::MAX)
}

/// Parses `raw`, with segments of about `segment` bytes in [`Stats`].
pub fn parse_segments(raw: &[u8], segment: usize) -> Result<Flat> {
    parse_segments_max(raw, segment, usize::MAX, 0, false)
}

/// [`parse_segments`] refusing data of more than `max` bytes (as [`parse_head_max`] does); the
/// data written to aqr-formula's store as it is made where `store` (with the feature `encode`),
/// rather than held.
pub fn parse_segments_max(
    raw: &[u8],
    segment: usize,
    max: usize,
    size: usize,
    store: bool,
) -> Result<Flat> {
    collect(
        raw,
        segment,
        max,
        Collect::<false>::new(room(raw, max, size), raw.len(), usize::MAX, store),
    )
}

/// [`parse_segments`] keeping only the tokens that start before `prefix` bytes of data, and
/// in runs further on ([`Flat::chunks`]): all else (the data, the blocks, the statistics) as
/// it gives; refusing data of more than `max` bytes; the data written to aqr-formula's store as
/// it is made where `store` (as [`parse_segments_max`]).
pub fn parse_segments_head(
    raw: &[u8],
    segment: usize,
    prefix: usize,
    max: usize,
    size: usize,
    store: bool,
) -> Result<Flat> {
    collect(
        raw,
        segment,
        max,
        Collect::<true>::new(room(raw, max, size), prefix / 2 + 64, prefix, store),
    )
}

/// The bytes to make room for at first for the data of `raw`, of at most `max` bytes: `size`,
/// where the container tells it (0: it does not), as DEFLATE can tell (at most 1032 bytes of
/// data a byte); else three times `raw`. (More is made room for as it comes.)
fn room(raw: &[u8], max: usize, size: usize) -> usize {
    match size {
        0 => raw.len().saturating_mul(3),
        n => n.min(max).min(raw.len().saturating_mul(1032)),
    }
}

fn collect<const HEAD: bool>(
    raw: &[u8],
    segment: usize,
    max: usize,
    mut c: Collect<HEAD>,
) -> Result<Flat> {
    let l = parse_head_max(raw, segment, max, &mut c);
    let (data, tokens, chunks) = c.into_vecs();
    let mut l = l?;
    l.stream.trailing = raw[l.end..].to_vec();
    Ok(Flat {
        stream: l.stream,
        data,
        tokens,
        starts: l.starts,
        skips: l.skips,
        stats: l.stats,
        chunks,
    })
}

/// Parses `raw` like [`parse_segments`], handing the data and tokens to `sink`.
pub fn parse_into<S: Sink>(raw: &[u8], segment: usize, sink: &mut S) -> Result<Layout> {
    let mut l = parse_head(raw, segment, sink)?;
    l.stream.trailing = raw[l.end..].to_vec();
    Ok(l)
}

/// [`parse_into`] for the stream at the start of `raw` alone: its trailing bytes are left empty
/// (where they start is [`Layout::end`]), so that what follows the stream is not copied. For a
/// stream found in data whose end is not known.
pub fn parse_head<S: Sink>(raw: &[u8], segment: usize, sink: &mut S) -> Result<Layout> {
    parse_head_max(raw, segment, usize::MAX, sink)
}

/// [`parse_head`] refusing data of more than `max` bytes ([`Error::Limit`]), before it is
/// all made (it goes at most a match beyond).
pub fn parse_head_max<S: Sink>(
    raw: &[u8],
    segment: usize,
    max: usize,
    sink: &mut S,
) -> Result<Layout> {
    let mut b = Bits {
        d: raw,
        pos: 0,
        buf: 0,
        n: 0,
    };
    let mut blocks = vec![];
    let mut starts = vec![];
    let mut skips = vec![];
    let mut st = Stats {
        cuts: vec![0],
        cut_pos: vec![0],
        segment,
        next: segment.min(max.saturating_add(1)),
        no_cut: usize::MAX,
        max,
        ..Default::default()
    };
    // Bytes and tokens so far.
    let (mut olen, mut tlen) = (0usize, 0usize);
    static FIXED: std::sync::OnceLock<(Table, Table)> = std::sync::OnceLock::new();
    let fixed = FIXED.get_or_init(|| {
        (
            Table::new(&huffman::fixed_lit_lens(), LIT_BITS).unwrap(),
            Table::new(&huffman::FIXED_DIST_LENS, DIST_BITS).unwrap(),
        )
    });
    let (mut lit, mut dist) = (Table::empty(LIT_BITS), Table::empty(DIST_BITS));
    loop {
        let last = b.get(1) == 1;
        starts.push(tlen);
        let body = match b.get(2) {
            0 => {
                let k = (b.bit_pos() as u32).wrapping_neg() & 7;
                let pad = b.get(k) as u8;
                let len = b.get(16) as u16;
                let nlen = b.get(16) as u16;
                b.check()?;
                if nlen != !len {
                    return Err(Error::Invalid("stored block length mismatch"));
                }
                // Byte aligned here: take the bytes directly.
                let at = b.bit_pos() / 8;
                let bytes = raw.get(at..at + len as usize).ok_or(Error::Truncated)?;
                sink.stored(bytes);
                olen += len as usize;
                b = Bits {
                    d: raw,
                    pos: at + len as usize,
                    buf: 0,
                    n: 0,
                };
                skips.push((tlen, len as usize));
                st.no_cut = tlen;
                st.freqs.push(([0; 288], [0; 32]));
                Body::Stored { pad, len }
            }
            1 => {
                tokens_into(
                    &mut b, &fixed.0, &fixed.1, sink, &mut olen, &mut tlen, &mut st,
                )?;
                Body::Fixed { tokens: vec![] }
            }
            2 => {
                let header = parse_header(&mut b)?;
                lit.build(header.lit_lens())?;
                dist.build(header.dist_lens())?;
                tokens_into(&mut b, &lit, &dist, sink, &mut olen, &mut tlen, &mut st)?;
                Body::Dynamic {
                    header,
                    tokens: vec![],
                }
            }
            _ => return Err(Error::Invalid("reserved block type")),
        };
        b.check()?;
        if olen > max {
            return Err(Error::Limit);
        }
        blocks.push(Block { last, body });
        if last {
            break;
        }
    }
    starts.push(tlen);
    st.cuts.push(tlen);
    let k = (b.bit_pos() as u32).wrapping_neg() & 7;
    let tail_pad = b.get(k) as u8;
    b.check()?;
    Ok(Layout {
        stream: Stream {
            blocks,
            tail_pad,
            trailing: Vec::new(),
        },
        starts,
        skips,
        stats: st,
        len: olen,
        end: b.bit_pos() / 8,
    })
}

/// A canonical Huffman code, decoded a bit at a time (as zlib's puff does): for finding
/// streams, where tables would cost more than the few symbols read.
struct Canon {
    count: [u16; 16],
    sorted: [u16; 320],
}

impl Canon {
    /// The code of `lens`, if zlib would take it: not over-subscribed, and complete unless
    /// `single` and it has one code of one bit. `None` also where it has no code at all.
    fn new(lens: &[u8], single: bool) -> Option<Canon> {
        let mut c = Canon {
            count: [0; 16],
            sorted: [0; 320],
        };
        lens.iter().for_each(|&l| c.count[l as usize] += 1);
        c.count[0] = 0;
        let max = (1..16).rev().find(|&l| c.count[l] > 0)?;
        let mut left = 1i32;
        for l in 1..16 {
            left = left * 2 - c.count[l] as i32;
            if left < 0 {
                return None;
            }
        }
        if left > 0 && !(single && max == 1) {
            return None;
        }
        let mut offs = [0u16; 16];
        for l in 1..15 {
            offs[l + 1] = offs[l] + c.count[l];
        }
        for (sym, &l) in lens.iter().enumerate() {
            if l > 0 {
                c.sorted[offs[l as usize] as usize] = sym as u16;
                offs[l as usize] += 1;
            }
        }
        Some(c)
    }

    fn decode(&self, b: &mut Bits) -> Option<u16> {
        let (mut code, mut first, mut index) = (0i32, 0i32, 0i32);
        for l in 1..16 {
            code |= b.get(1) as i32;
            let n = self.count[l] as i32;
            if code - first < n {
                return Some(self.sorted[(index + code - first) as usize]);
            }
            index += n;
            first = (first + n) << 1;
            code <<= 1;
        }
        None
    }
}

/// Tokens of a block decoded by [`plausible_start`]: enough that data which only looks like a
/// fixed block's start fails, as its matches reach before the stream.
const PLAUSIBLE_TOKENS: usize = 32;

/// Whether `raw` starts like a DEFLATE stream, by its first block, without decoding its data:
/// for finding streams in other data, where a header that only looks right is common (a
/// two-byte zlib header passes once in about 250 places, and more often in code). A stored
/// block's length has its complement after it. A dynamic block has codes zlib takes (after
/// rapidgzip's block finder, arXiv:2308.08955): a complete code-length code, code lengths it
/// decodes into without running over, an end-of-block code, and complete literal/length and
/// distance codes (or of one code; no distance code at all is allowed too). The first tokens
/// of a fixed or dynamic block must be valid, their matches within the bytes before them.
///
/// A fixed block is taken only with `fixed`: a quarter of all positions have its type, and a
/// last one of a few literals is as likely in noise (one place in 200) as it is real. Streams
/// found in the middle of data are better without it: those that start with one hold a few
/// hundred bytes at most.
pub fn plausible_start(raw: &[u8], fixed: bool) -> bool {
    let mut b = Bits {
        d: raw,
        pos: 0,
        buf: 0,
        n: 0,
    };
    let (mut out, mut tokens) = (0usize, 0);
    // block after block while tokens are left: a block that is not the last does not end
    // the stream
    loop {
        let last = b.get(1) == 1;
        let codes = match b.get(2) {
            0 => {
                let k = (b.bit_pos() as u32).wrapping_neg() & 7;
                b.get(k);
                let (len, nlen) = (b.get(16), b.get(16));
                return len == !nlen & 0xffff && b.check().is_ok();
            }
            1 if fixed => {
                let mut lens = [8u8; 288];
                lens[144..256].fill(9);
                lens[256..280].fill(7);
                Canon::new(&lens, false).zip(Canon::new(&[5; 32], false))
            }
            2 => plausible_codes(&mut b),
            _ => None,
        };
        let Some((lit, dist)) = codes else {
            return false;
        };
        loop {
            if tokens == PLAUSIBLE_TOKENS {
                return true;
            }
            tokens += 1;
            let ok = match lit.decode(&mut b) {
                Some(0..=255) => {
                    out += 1;
                    true
                }
                Some(256) if last => return b.check().is_ok(),
                Some(256) => break,
                Some(s @ 257..=285) => {
                    let len = LEN_BASE[s as usize - 257] as usize;
                    b.get(LEN_EXTRA[s as usize - 257] as u32);
                    let d = dist.decode(&mut b).filter(|&d| d < 30).map(|d| {
                        DIST_BASE[d as usize] as usize
                            + b.get(DIST_EXTRA[d as usize] as u32) as usize
                    });
                    let within = d.is_some_and(|d| d <= out);
                    out += len;
                    within
                }
                _ => false,
            };
            if !ok || b.check().is_err() {
                return false;
            }
        }
        if b.check().is_err() {
            return false;
        }
    }
}

/// The codes of a dynamic block's header, if zlib would take them (see [`plausible_start`]).
fn plausible_codes(b: &mut Bits) -> Option<(Canon, Canon)> {
    let hlit = b.get(5) as usize + 257;
    let hdist = b.get(5) as usize + 1;
    let hclen = b.get(4) as usize + 4;
    if hlit > 286 || hdist > 30 {
        return None;
    }
    let mut cl = [0u8; 19];
    for &s in &CL_ORDER[..hclen] {
        cl[s] = b.get(3) as u8;
    }
    let cl = Canon::new(&cl, false)?;
    let mut lens = [0u8; 316];
    let mut n = 0;
    while n < hlit + hdist {
        let (len, rep) = match cl.decode(b)? {
            s @ 0..=15 => (s as u8, 1),
            16 => (*lens.get(n.checked_sub(1)?)?, 3 + b.get(2) as usize),
            17 => (0, 3 + b.get(3) as usize),
            _ => (0, 11 + b.get(7) as usize),
        };
        if n + rep > hlit + hdist || b.check().is_err() {
            return None;
        }
        lens[n..n + rep].fill(len);
        n += rep;
    }
    let (lit, dist) = lens[..hlit + hdist].split_at(hlit);
    if lit[256] == 0 {
        return None;
    }
    let dist = if dist.iter().all(|&l| l == 0) {
        // no match can be decoded: an empty code
        Canon {
            count: [0; 16],
            sorted: [0; 320],
        }
    } else {
        Canon::new(dist, true)?
    };
    Some((Canon::new(lit, true)?, dist))
}

fn parse_header(b: &mut Bits) -> Result<DynHeader> {
    let hlit = b.get(5) as u16 + 257;
    let hdist = b.get(5) as u8 + 1;
    let hclen = b.get(4) as u8 + 4;
    let mut cl_lens = [0u8; 19];
    for &s in &CL_ORDER[..hclen as usize] {
        cl_lens[s] = b.get(3) as u8;
    }
    let cl = Table::new(&cl_lens, CL_BITS)?;
    let total = hlit as usize + hdist as usize;
    let mut ops = Vec::with_capacity(total);
    let mut n = 0;
    while n < total {
        b.refill();
        let sym = cl.decode(b)? as u8;
        let extra = b.bits(RleOp::extra_bits(sym)) as u8;
        let op = RleOp { sym, extra };
        n += op.count();
        ops.push(op);
        b.check()?;
    }
    let lens = huffman::rle_expand(&ops, total)?;
    if lens[256] == 0 {
        return Err(Error::Invalid("no end-of-block code"));
    }
    Ok(DynHeader {
        hlit,
        hdist,
        hclen,
        cl_lens,
        ops,
        lens,
    })
}

#[inline(always)]
fn tokens_into<S: Sink>(
    bits: &mut Bits,
    lit: &Table,
    dist: &Table,
    sink: &mut S,
    olen: &mut usize,
    tlen: &mut usize,
    st: &mut Stats,
) -> Result<()> {
    let mut b = *bits;
    let (mut lf, mut df) = ([0u32; 288], [0u32; 32]);
    let (mut max_dist, mut far) = (st.max_dist, st.far);
    let (mut o, mut t) = (*olen, *tlen);
    let r = loop {
        sink.reserve();
        // A token takes at most 15 + 5 + 15 + 13 = 48 bits.
        b.refill();
        if b.pos > b.d.len() + 16 {
            break Err(Error::Truncated);
        }
        if o >= st.next {
            if o > st.max {
                break Err(Error::Limit);
            }
            st.token_at(t, o);
        }
        let sym = match lit.decode(&mut b) {
            Ok(s) => s,
            Err(e) => break Err(e),
        };
        lf[sym as usize] += 1;
        if sym < 256 {
            sink.lit(sym as u8);
            o += 1;
            t += 1;
            // Two more literals fit in the bits left (at least 56 - 15, and these come
            // from the primary table), unless a segment may start before them.
            if o + 2 < st.next {
                for _ in 0..2 {
                    let e = lit.t[(b.buf & lit.mask) as usize];
                    let sym = e & 0xffff;
                    if e & SUB != 0 || sym >= 256 || e >> 16 == 0 {
                        break;
                    }
                    b.bits(e >> 16);
                    lf[sym as usize] += 1;
                    sink.lit(sym as u8);
                    o += 1;
                    t += 1;
                }
            }
            continue;
        }
        if sym == 256 {
            break b.check();
        }
        let c = sym as usize - 257;
        if c >= 29 {
            break Err(Error::Invalid("invalid literal/length symbol"));
        }
        let e = b.bits(LEN_EXTRA[c] as u32) as u16;
        let len = LEN_BASE[c] + e;
        let alt258 = c == 27 && e == 31;
        let dc = match dist.decode(&mut b) {
            Ok(s) => s as usize,
            Err(e) => break Err(e),
        };
        if dc >= 30 {
            break Err(Error::Invalid("invalid distance symbol"));
        }
        df[dc] += 1;
        b.refill();
        let d = DIST_BASE[dc] as usize + b.bits(DIST_EXTRA[dc] as u32) as usize;
        if d > o {
            break b
                .check()
                .and(Err(Error::Invalid("distance before start of data")));
        }
        max_dist = max_dist.max(d as u16);
        // Length codes 0 and 1 are lengths 3 and 4; distance codes 24 and 26 start past
        // 4096 and 8192.
        far |= (c == 0 && dc >= 24) as u8
            | ((c == 0 && dc >= 26) as u8) << 1
            | ((c <= 1 && dc >= 24) as u8) << 2;
        sink.mat(len, d as u16, alt258);
        o += len as usize;
        t += 1;
    };
    (*olen, *tlen) = (o, t);
    *bits = b;
    st.freqs.push((lf, df));
    (st.max_dist, st.far) = (max_dist, far);
    r
}

/// Writes the `len` bytes `d` back at `base + n` (`1 <= d <= n`, room for `len` rounded up
/// to 16 past `n`), in 16-byte chunks: a library call per match would cost more than the
/// copy itself.
///
/// # Safety
/// As stated: `base[..n]` is initialized and `base[n..n + len + 16]` is allocated.
#[inline(always)]
pub unsafe fn copy_match(base: *mut u8, n: usize, d: usize, len: usize) {
    // Every chunk read lies before the chunk written (16 bytes with `d >= 16`, 8 otherwise)
    // and within what is initialized: `src + i + chunk <= dst + i`. A distance of 1 repeats
    // a byte. Shorter ones than 8 repeat a pattern of period `d`: once its first `dd` bytes
    // are written (`dd`, a multiple of `d`, at least 8), the rest is copied from `dd` back.
    unsafe {
        let dst = base.add(n);
        if d >= 16 {
            let src = base.add(n - d);
            let mut i = 0;
            while i < len {
                std::ptr::copy_nonoverlapping(src.add(i), dst.add(i), 16);
                i += 16;
            }
        } else if d >= 8 {
            let src = base.add(n - d);
            let mut i = 0;
            while i < len {
                std::ptr::copy_nonoverlapping(src.add(i), dst.add(i), 8);
                i += 8;
            }
        } else if d == 1 {
            std::ptr::write_bytes(dst, *dst.sub(1), len);
        } else {
            let dd = d * 8usize.div_ceil(d);
            let head = dd.min(len);
            for i in 0..head {
                *dst.add(i) = *dst.add(i).sub(d);
            }
            let mut i = head;
            while i < len {
                std::ptr::copy_nonoverlapping(dst.add(i - dd), dst.add(i), 8);
                i += 8;
            }
        }
    }
}

/// The data of the raw DEFLATE stream `raw`, which must end where `raw` does (but for the bits
/// padding its last byte), and hold exactly `len` bytes: the decoding of the module
/// `flate-inflate` (FORMAT.md, "The module flate-inflate"). Only the data is made, into a buffer
/// of `len` bytes (nothing of the blocks or tokens is kept), so that its memory is the output's.
pub fn data(raw: &[u8], len: usize) -> Result<Vec<u8>> {
    let mut b = Bits {
        d: raw,
        pos: 0,
        buf: 0,
        n: 0,
    };
    let mut out: Vec<u8> = Vec::new();
    out.try_reserve_exact(len).map_err(|_| Error::Limit)?;
    static FIXED: std::sync::OnceLock<(Table, Table)> = std::sync::OnceLock::new();
    let fixed = FIXED.get_or_init(|| {
        (
            Table::new(&huffman::fixed_lit_lens(), LIT_BITS).unwrap(),
            Table::new(&huffman::FIXED_DIST_LENS, DIST_BITS).unwrap(),
        )
    });
    let (mut lit, mut dist) = (Table::empty(LIT_BITS), Table::empty(DIST_BITS));
    loop {
        let last = b.get(1) == 1;
        match b.get(2) {
            0 => {
                let k = (b.bit_pos() as u32).wrapping_neg() & 7;
                b.get(k);
                let n = b.get(16) as u16;
                let nn = b.get(16) as u16;
                b.check()?;
                if nn != !n {
                    return Err(Error::Invalid("stored block length mismatch"));
                }
                let at = b.bit_pos() / 8;
                let bytes = raw.get(at..at + n as usize).ok_or(Error::Truncated)?;
                if out.len() + bytes.len() > len {
                    return Err(Error::Limit);
                }
                out.extend_from_slice(bytes);
                b = Bits {
                    d: raw,
                    pos: at + n as usize,
                    buf: 0,
                    n: 0,
                };
            }
            1 => data_tokens(&mut b, &fixed.0, &fixed.1, &mut out, len)?,
            2 => {
                let header = parse_header(&mut b)?;
                lit.build(header.lit_lens())?;
                dist.build(header.dist_lens())?;
                data_tokens(&mut b, &lit, &dist, &mut out, len)?;
            }
            _ => return Err(Error::Invalid("reserved block type")),
        }
        b.check()?;
        if last {
            break;
        }
    }
    let k = (b.bit_pos() as u32).wrapping_neg() & 7;
    b.get(k);
    b.check()?;
    if b.bit_pos() / 8 != raw.len() {
        return Err(Error::Invalid("bytes after the stream"));
    }
    if out.len() != len {
        return Err(Error::Corrupt);
    }
    Ok(out)
}

/// Bytes of the data [`data_streamed`] keeps back for the distances of matches.
const HISTORY: usize = 32 << 10;

/// Bytes [`data_streamed`] makes at most before it writes them out and lets go of all but the
/// last [`HISTORY`] of them.
const FLUSH: usize = 256 << 10;

/// Bytes of the stream [`data_streamed`] has ahead of where it decodes, where there are as many:
/// before a token (of at most 48 bits, and the 8 bytes a refill loads) and before a block's
/// header (a dynamic one of at most some 560 bytes).
const AHEAD_TOKEN: usize = 32;
const AHEAD_HEADER: usize = 1024;

/// Bytes [`data_streamed`] reads at most at once.
const READ: usize = 64 << 10;

/// Bytes [`data_streamed`] holds at most of the stream and of the data: what it reads at once
/// beside those ahead, and [`HISTORY`] and [`FLUSH`] beside a match (at most 258 bytes).
pub const INPUT: usize = READ + AHEAD_HEADER;
pub const OUTPUT: usize = HISTORY + FLUSH + 258;

/// [`data`] of a stream read a piece at a time and its data written out a piece at a time,
/// holding a piece of each and [`HISTORY`] bytes of the data rather than either whole: `read`
/// appends the next bytes of the stream (`len_in` bytes in all) to the vector it is given, at
/// most so many as it takes from the end of the vector (at least one), and `write` takes the
/// data in order (`len` bytes in all), in pieces. The decoding of the module `flate-inflate` as
/// a part module.
pub fn data_streamed(
    len_in: u64,
    read: &mut dyn FnMut(&mut Vec<u8>, usize) -> Result<()>,
    write: &mut dyn FnMut(&[u8]) -> Result<()>,
    len: u64,
) -> Result<()> {
    let mut s = Streamed {
        input: Vec::with_capacity(INPUT),
        taken: 0,
        len_in,
        state: (0, 0, 0),
        out: Vec::with_capacity(OUTPUT),
        written: 0,
        made: 0,
        len,
        read,
        write,
    };
    static FIXED: std::sync::OnceLock<(Table, Table)> = std::sync::OnceLock::new();
    let fixed = FIXED.get_or_init(|| {
        (
            Table::new(&huffman::fixed_lit_lens(), LIT_BITS).unwrap(),
            Table::new(&huffman::FIXED_DIST_LENS, DIST_BITS).unwrap(),
        )
    });
    let (mut lit, mut dist) = (Table::empty(LIT_BITS), Table::empty(DIST_BITS));
    loop {
        s.ahead(AHEAD_HEADER)?;
        let mut b = s.bits();
        let last = b.get(1) == 1;
        match b.get(2) {
            0 => {
                let k = (b.bit_pos() as u32).wrapping_neg() & 7;
                b.get(k);
                let n = b.get(16) as u16;
                let nn = b.get(16) as u16;
                b.check()?;
                if nn != !n {
                    return Err(Error::Invalid("stored block length mismatch"));
                }
                // (from the byte after the lengths, the bits loaded ahead put back)
                s.state = (b.bit_pos() / 8, 0, 0);
                let mut left = n as usize;
                while left > 0 {
                    s.flush(false)?;
                    s.ahead(1)?;
                    let at = s.state.0;
                    let k = left
                        .min(s.input.len() - at)
                        .min(HISTORY + FLUSH - s.out.len());
                    if k == 0 {
                        return Err(Error::Truncated);
                    }
                    if s.made + k as u64 > s.len {
                        return Err(Error::Limit);
                    }
                    s.out.extend_from_slice(&s.input[at..at + k]);
                    s.made += k as u64;
                    s.state.0 += k;
                    left -= k;
                }
            }
            1 => {
                s.state = (b.pos, b.buf, b.n);
                s.tokens(&fixed.0, &fixed.1)?;
            }
            2 => {
                let header = parse_header(&mut b)?;
                lit.build(header.lit_lens())?;
                dist.build(header.dist_lens())?;
                s.state = (b.pos, b.buf, b.n);
                s.tokens(&lit, &dist)?;
            }
            _ => return Err(Error::Invalid("reserved block type")),
        }
        s.bits().check()?;
        if last {
            break;
        }
    }
    let mut b = s.bits();
    let k = (b.bit_pos() as u32).wrapping_neg() & 7;
    b.get(k);
    b.check()?;
    if s.taken + (b.bit_pos() / 8) as u64 != s.len_in || b.bit_pos() / 8 != s.input.len() {
        return Err(Error::Invalid("bytes after the stream"));
    }
    if s.made != s.len {
        return Err(Error::Corrupt);
    }
    s.flush(true)
}

/// The state of [`data_streamed`].
struct Streamed<'r> {
    /// The bytes of the stream read and not yet let go of, those before them taken (decoded),
    /// and the stream's length.
    input: Vec<u8>,
    taken: u64,
    len_in: u64,
    /// Where it decodes in `input`: [`Bits`]'s next byte to load, and the bits loaded.
    state: (usize, u64, u32),
    /// The data made and not let go of, of which the first `written` are written out, and the
    /// data made in all, of `len` bytes at most.
    out: Vec<u8>,
    written: usize,
    made: u64,
    len: u64,
    read: &'r mut dyn FnMut(&mut Vec<u8>, usize) -> Result<()>,
    write: &'r mut dyn FnMut(&[u8]) -> Result<()>,
}

impl Streamed<'_> {
    fn bits(&self) -> Bits<'_> {
        let (pos, buf, n) = self.state;
        Bits {
            d: &self.input,
            pos,
            buf,
            n,
        }
    }

    /// At least `n` bytes of the stream read ahead of where it decodes, or all that is left of
    /// it: the bytes before let go of first.
    fn ahead(&mut self, n: usize) -> Result<()> {
        let at = self.state.0;
        if self.input.len() - at.min(self.input.len()) >= n {
            return Ok(());
        }
        // (but those of the bits loaded, for the bits consumed to be counted from within them)
        let gone = at
            .min(self.input.len())
            .saturating_sub((self.state.2 as usize).div_ceil(8));
        self.input.drain(..gone);
        self.taken += gone as u64;
        self.state.0 -= gone;
        while self.input.len() - self.state.0.min(self.input.len()) < n {
            let left = self.len_in - self.taken - self.input.len() as u64;
            if left == 0 {
                break;
            }
            let k = (n.max(READ) as u64).min(left) as usize;
            let before = self.input.len();
            (self.read)(&mut self.input, k)?;
            if self.input.len() == before || self.input.len() > before + k {
                return Err(Error::Corrupt);
            }
        }
        Ok(())
    }

    /// The data made since written out (`all`: or where it has more than [`FLUSH`] bytes), all
    /// but the last [`HISTORY`] of it let go of.
    fn flush(&mut self, all: bool) -> Result<()> {
        if !all && self.out.len() < HISTORY + FLUSH {
            return Ok(());
        }
        (self.write)(&self.out[self.written..])?;
        let gone = self.out.len().saturating_sub(HISTORY);
        self.out.drain(..gone);
        self.written = self.out.len();
        Ok(())
    }

    /// A block's tokens up to its end of block, of tables `lit` and `dist`, the data written
    /// out as it grows.
    fn tokens(&mut self, lit: &Table, dist: &Table) -> Result<()> {
        loop {
            self.ahead(AHEAD_TOKEN)?;
            self.flush(false)?;
            let (pos, buf, n) = self.state;
            let mut b = Bits {
                d: &self.input,
                pos,
                buf,
                n,
            };
            // (as many tokens as the stream read ahead and the room before a flush allow)
            let stop = self.input.len().saturating_sub(AHEAD_TOKEN);
            let room = HISTORY + FLUSH;
            let r = loop {
                if b.pos > stop || self.out.len() >= room {
                    break Ok(false);
                }
                b.refill();
                if b.pos > b.d.len() + 16 {
                    break Err(Error::Truncated);
                }
                let sym = match lit.decode(&mut b) {
                    Ok(s) => s,
                    Err(e) => break Err(e),
                };
                if sym < 256 {
                    if self.made >= self.len {
                        break Err(Error::Limit);
                    }
                    self.out.push(sym as u8);
                    self.made += 1;
                    continue;
                }
                if sym == 256 {
                    break b.check().map(|()| true);
                }
                let c = sym as usize - 257;
                if c >= 29 {
                    break Err(Error::Invalid("invalid literal/length symbol"));
                }
                let n = (LEN_BASE[c] + b.bits(LEN_EXTRA[c] as u32) as u16) as usize;
                let dc = match dist.decode(&mut b) {
                    Ok(s) => s as usize,
                    Err(e) => break Err(e),
                };
                if dc >= 30 {
                    break Err(Error::Invalid("invalid distance symbol"));
                }
                b.refill();
                let d = DIST_BASE[dc] as usize + b.bits(DIST_EXTRA[dc] as u32) as usize;
                if d as u64 > self.made.min(self.out.len() as u64) {
                    break b
                        .check()
                        .and(Err(Error::Invalid("distance before start of data")));
                }
                if self.made + n as u64 > self.len {
                    break Err(Error::Limit);
                }
                let from = self.out.len() - d;
                if d >= n {
                    self.out.extend_from_within(from..from + n);
                } else {
                    // (overlapping: a run of the last `d` bytes)
                    for i in 0..n {
                        self.out.push(self.out[from + i]);
                    }
                }
                self.made += n as u64;
            };
            self.state = (b.pos, b.buf, b.n);
            match r {
                Ok(true) => return Ok(()),
                Ok(false)
                    if self.taken + self.input.len() as u64 == self.len_in && b.pos > stop =>
                {
                    // (the end of the stream within the bytes ahead: decoded with it as they are)
                    return self.tail(lit, dist);
                }
                Ok(false) => {}
                Err(e) => return Err(e),
            }
        }
    }

    /// [`Streamed::tokens`] of the last bytes of the stream, all of them read.
    fn tail(&mut self, lit: &Table, dist: &Table) -> Result<()> {
        loop {
            self.flush(false)?;
            let (pos, buf, n) = self.state;
            let mut b = Bits {
                d: &self.input,
                pos,
                buf,
                n,
            };
            b.refill();
            if b.pos > b.d.len() + 16 {
                return Err(Error::Truncated);
            }
            let sym = lit.decode(&mut b)?;
            let r = if sym < 256 {
                if self.made >= self.len {
                    return Err(Error::Limit);
                }
                self.out.push(sym as u8);
                self.made += 1;
                false
            } else if sym == 256 {
                b.check()?;
                true
            } else {
                let c = sym as usize - 257;
                if c >= 29 {
                    return Err(Error::Invalid("invalid literal/length symbol"));
                }
                let n = (LEN_BASE[c] + b.bits(LEN_EXTRA[c] as u32) as u16) as usize;
                let dc = dist.decode(&mut b)? as usize;
                if dc >= 30 {
                    return Err(Error::Invalid("invalid distance symbol"));
                }
                b.refill();
                let d = DIST_BASE[dc] as usize + b.bits(DIST_EXTRA[dc] as u32) as usize;
                if d as u64 > self.made.min(self.out.len() as u64) {
                    b.check()?;
                    return Err(Error::Invalid("distance before start of data"));
                }
                if self.made + n as u64 > self.len {
                    return Err(Error::Limit);
                }
                let from = self.out.len() - d;
                for i in 0..n {
                    self.out.push(self.out[from + i]);
                }
                self.made += n as u64;
                false
            };
            self.state = (b.pos, b.buf, b.n);
            if r {
                return Ok(());
            }
        }
    }
}

/// The data of a block's tokens (up to its end of block) for [`data`], appended to `out`,
/// which may not grow past `len` bytes.
fn data_tokens(
    bits: &mut Bits,
    lit: &Table,
    dist: &Table,
    out: &mut Vec<u8>,
    len: usize,
) -> Result<()> {
    let mut b = *bits;
    let r = loop {
        // A token takes at most 15 + 5 + 15 + 13 = 48 bits.
        b.refill();
        if b.pos > b.d.len() + 16 {
            break Err(Error::Truncated);
        }
        let sym = match lit.decode(&mut b) {
            Ok(s) => s,
            Err(e) => break Err(e),
        };
        if sym < 256 {
            if out.len() >= len {
                break Err(Error::Limit);
            }
            out.push(sym as u8);
            continue;
        }
        if sym == 256 {
            break b.check();
        }
        let c = sym as usize - 257;
        if c >= 29 {
            break Err(Error::Invalid("invalid literal/length symbol"));
        }
        let n = (LEN_BASE[c] + b.bits(LEN_EXTRA[c] as u32) as u16) as usize;
        let dc = match dist.decode(&mut b) {
            Ok(s) => s as usize,
            Err(e) => break Err(e),
        };
        if dc >= 30 {
            break Err(Error::Invalid("invalid distance symbol"));
        }
        b.refill();
        let d = DIST_BASE[dc] as usize + b.bits(DIST_EXTRA[dc] as u32) as usize;
        if d > out.len() {
            break b
                .check()
                .and(Err(Error::Invalid("distance before start of data")));
        }
        if out.len() + n > len {
            break Err(Error::Limit);
        }
        let from = out.len() - d;
        if d >= n {
            out.extend_from_within(from..from + n);
        } else {
            // (overlapping: a run of the last `d` bytes)
            for i in 0..n {
                out.push(out[from + i]);
            }
        }
    };
    *bits = b;
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parsing with the data written out as it is made (through a window) gives what parsing
    /// it whole does: the data, the tokens kept, their runs, the cuts, the stored blocks, the
    /// bytes after the stream.
    #[test]
    fn written_out_as_whole() {
        use crate::zlib::{Params, compress};
        let text: Vec<u8> = (0..300_000)
            .flat_map(|i| format!("line {} of {}\n", i % 977, i % 13).into_bytes())
            .collect();
        let mut d = text.clone();
        d.extend(aqr_common::rng::noise(1_500_000));
        d.extend(&text[..2_000_000]);
        for level in [1, 6, 9] {
            let raw = compress(
                &d,
                Params {
                    level,
                    ..Params::default()
                },
            )
            .unwrap();
            let raw = [&raw[..], b"after"].concat();
            for head in [false, true] {
                let parse = |store| match head {
                    true => parse_segments_head(&raw, 1 << 20, 16 << 10, usize::MAX, 0, store),
                    false => parse_segments_max(&raw, 1 << 20, usize::MAX, 0, store),
                };
                let (a, b) = (parse(false).unwrap(), parse(true).unwrap());
                assert_eq!(a.data, d);
                assert!(!a.skips.is_empty() && a.data.len() > 4 * STORE_FROM);
                assert!(a.data == b.data, "level {level}");
                assert_eq!(a.tokens, b.tokens);
                assert_eq!(a.chunks, b.chunks);
                assert_eq!(a.skips, b.skips);
                assert_eq!(
                    (a.stats.cuts, a.stats.cut_pos),
                    (b.stats.cuts, b.stats.cut_pos)
                );
                assert_eq!(a.stream.trailing, b.stream.trailing);
            }
        }
    }

    /// [`data`] gives the data of streams of every block type (fixed, dynamic, stored; matches
    /// overlapping themselves), as [`parse`] does, and refuses a length not the data's, bytes
    /// after the stream, and a stream cut short.
    #[test]
    fn data_only() {
        use crate::zlib::{Params, Strategy, compress};
        let text: Vec<u8> = (0..20000)
            .flat_map(|i| format!("line {} of {}\n", i % 97, i % 13).into_bytes())
            .collect();
        let runs: Vec<u8> = (0..50000).map(|i| (i / 1000 % 3) as u8).collect();
        let samples = [
            text,
            runs,
            aqr_common::rng::noise(100_000),
            b"ab".to_vec(),
            Vec::new(),
        ];
        for d in &samples {
            for (level, strategy) in [
                (1, Strategy::Default),
                (6, Strategy::Default),
                (9, Strategy::Fixed),
                (6, Strategy::Rle),
            ] {
                let raw = compress(
                    d,
                    Params {
                        level,
                        strategy,
                        ..Params::default()
                    },
                )
                .unwrap();
                assert_eq!(parse(&raw).unwrap().data, *d);
                assert_eq!(
                    data(&raw, d.len()).unwrap(),
                    *d,
                    "{} {level} {strategy:?}",
                    d.len()
                );
                assert!(data(&raw, d.len() + 1).is_err());
                assert!(d.is_empty() || data(&raw, d.len() - 1).is_err());
                assert!(data(&[&raw[..], &[0]].concat(), d.len()).is_err());
                assert!(data(&raw[..raw.len() - 1], d.len()).is_err());
            }
        }
    }

    /// [`data_streamed`] of the stream read in pieces of `piece` bytes at most: the data, and
    /// the largest piece written.
    fn streamed(raw: &[u8], len: usize, piece: usize) -> Result<(Vec<u8>, usize)> {
        let (mut at, mut out, mut largest) = (0, Vec::new(), 0);
        data_streamed(
            raw.len() as u64,
            &mut |v: &mut Vec<u8>, n: usize| {
                let k = n.min(piece).min(raw.len() - at);
                v.extend_from_slice(&raw[at..at + k]);
                at += k;
                Ok(())
            },
            &mut |d: &[u8]| {
                largest = largest.max(d.len());
                out.extend_from_slice(d);
                Ok(())
            },
            len as u64,
        )?;
        Ok((out, largest))
    }

    /// [`data_streamed`] gives what [`data`] does, the stream read in pieces of any size and the
    /// data written in pieces of a bounded size (of blocks longer than that too), and refuses
    /// what it refuses.
    #[test]
    fn streamed_as_data() {
        use crate::zlib::{Params, Strategy, compress};
        let text: Vec<u8> = (0..20000)
            .flat_map(|i| format!("line {} of {}\n", i % 97, i % 13).into_bytes())
            .collect();
        let runs: Vec<u8> = (0..3_000_000).map(|i| (i / 100_000 % 3) as u8).collect();
        let samples = [
            text,
            runs,
            aqr_common::rng::noise(300_000),
            b"ab".to_vec(),
            Vec::new(),
        ];
        for d in &samples {
            for (level, strategy) in [
                (0, Strategy::Default),
                (1, Strategy::Default),
                (6, Strategy::Default),
                (9, Strategy::Fixed),
                (6, Strategy::Rle),
            ] {
                let raw = match level {
                    // stored blocks, as long as they may be
                    0 => {
                        let mut raw = Vec::new();
                        let blocks: Vec<&[u8]> = d.chunks(65535).collect();
                        for (k, b) in blocks
                            .iter()
                            .enumerate()
                            .chain((blocks.is_empty()).then_some((0, &&[][..])))
                        {
                            raw.push((k + 1 >= blocks.len()) as u8);
                            raw.extend_from_slice(&(b.len() as u16).to_le_bytes());
                            raw.extend_from_slice(&(!(b.len() as u16)).to_le_bytes());
                            raw.extend_from_slice(b);
                        }
                        raw
                    }
                    _ => compress(
                        d,
                        Params {
                            level,
                            strategy,
                            ..Params::default()
                        },
                    )
                    .unwrap(),
                };
                assert_eq!(data(&raw, d.len()).unwrap(), *d);
                for piece in [1, 7, 4096, usize::MAX] {
                    let what = format!("{} {level} {strategy:?} {piece}", d.len());
                    let (out, largest) =
                        streamed(&raw, d.len(), piece).unwrap_or_else(|e| panic!("{what}: {e:?}"));
                    assert!(out == *d, "{what}");
                    assert!(largest <= HISTORY + FLUSH + 258, "{what}: {largest}");
                    if piece == 7 || piece == usize::MAX {
                        assert!(streamed(&raw, d.len() + 1, piece).is_err(), "{what}");
                        assert!(
                            d.is_empty() || streamed(&raw, d.len() - 1, piece).is_err(),
                            "{what}"
                        );
                        assert!(
                            streamed(&[&raw[..], &[0]].concat(), d.len(), piece).is_err(),
                            "{what}"
                        );
                        assert!(
                            streamed(&raw[..raw.len() - 1], d.len(), piece).is_err(),
                            "{what}"
                        );
                    }
                }
            }
        }
    }

    struct Skip;

    impl Sink for Skip {
        fn lit(&mut self, _: u8) {}
        fn mat(&mut self, _: u16, _: u16, _: bool) {}
        fn stored(&mut self, _: &[u8]) {}
    }

    /// Every stream of the samples cached from flate-zoo's encoders starts plausibly (where
    /// they are cached: `aqr-flate-train samples`).
    #[test]
    fn plausible_samples() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/cache/samples");
        let Ok(dirs) = std::fs::read_dir(dir) else {
            return;
        };
        let mut n = 0;
        for d in dirs.flatten() {
            for f in std::fs::read_dir(d.path()).unwrap().flatten() {
                if f.path().extension().is_none_or(|e| e != "raw") {
                    continue;
                }
                let raw = std::fs::read(f.path()).unwrap();
                assert!(plausible_start(&raw, true), "{}", f.path().display());
                n += 1;
            }
        }
        assert!(n > 0);
    }

    /// Streams of every block type start plausibly; noise rarely does. What passes is nearly
    /// all a last fixed block of a few literals, as real streams of a few bytes are; a dynamic
    /// block hardly ever.
    #[test]
    fn plausible() {
        let data = include_bytes!("zlib.rs");
        let stored = crate::deflate::write(
            &Stream {
                blocks: vec![Block {
                    last: true,
                    body: Body::Stored { pad: 0, len: 5 },
                }],
                tail_pad: 0,
                trailing: vec![],
            },
            b"hello",
        )
        .unwrap();
        assert!(plausible_start(&stored, false));
        // fixed: an empty stream, and "aaaa..." as a literal and a match
        assert!(plausible_start(&[0x03, 0x00], true));
        assert!(plausible_start(&[0x4b, 0x4c, 0x84, 0x01, 0x00], true));
        assert!(!plausible_start(&[0x03, 0x00], false));
        let mut e = crate::zlib::Emulator::new(data, crate::zlib::Params::default());
        let g = crate::zlib::zlib_block(&mut e, 0).unwrap();
        let dynamic = crate::deflate::write(
            &Stream {
                blocks: vec![Block { last: true, ..g }],
                tail_pad: 0,
                trailing: vec![],
            },
            &data[..],
        );
        if let Ok(d) = dynamic {
            assert!(plausible_start(&d, false));
        }
        let noise = aqr_common::rng::noise(1 << 20);
        // by block type: stored, fixed, dynamic
        let mut by = [0usize; 3];
        for p in (0..noise.len() - 64).filter(|&p| plausible_start(&noise[p..], true)) {
            by[(noise[p] >> 1 & 3) as usize] += 1;
        }
        assert!(by.iter().sum::<usize>() < noise.len() / 100, "{by:?}");
        assert!(by[0] < 10 && by[2] < 10, "{by:?}");
        let n = (0..noise.len() - 64)
            .filter(|&p| plausible_start(&noise[p..], false))
            .count();
        assert_eq!(n, by[0] + by[2]);
    }

    /// `parse_head` ends where `parse_into` does, without the trailing bytes.
    #[test]
    fn head_without_trailing() {
        let mut raw = crate::deflate::write(
            &Stream {
                blocks: vec![Block {
                    last: true,
                    body: Body::Stored { pad: 0, len: 5 },
                }],
                tail_pad: 0,
                trailing: vec![],
            },
            b"hello",
        )
        .unwrap();
        let end = raw.len();
        raw.extend_from_slice(&[9; 1000]);
        let head = parse_head(&raw, usize::MAX, &mut Skip).unwrap();
        assert_eq!((head.end, head.len), (end, 5));
        assert!(head.stream.trailing.is_empty());
        let full = parse_into(&raw, usize::MAX, &mut Skip).unwrap();
        assert_eq!(full.end, end);
        assert_eq!(full.stream.trailing, [9; 1000]);
    }

    /// Gives what `deflate::parse` gives, on streams of every block type.
    #[test]
    fn same_as_reference_parser() {
        let mut lcg = 5u32;
        let mut data = include_bytes!("zlib.rs").to_vec();
        data.extend((0..70_000).map(|_| {
            lcg = lcg.wrapping_mul(1_103_515_245).wrapping_add(12345);
            (lcg >> 24) as u8
        }));
        data.extend(b"abcabcabd".repeat(5000));
        let stream = |blocks: Vec<Block>| Stream {
            blocks,
            tail_pad: 0,
            trailing: vec![7, 8],
        };
        // Stored, fixed and zlib's dynamic blocks over the same data.
        let mut e = crate::zlib::Emulator::new(&data, crate::zlib::Params::default());
        let mut blocks = vec![];
        let mut pos = 0;
        while let Some(g) = crate::zlib::zlib_block(&mut e, pos) {
            pos += match &g.body {
                Body::Stored { len, .. } => *len as usize,
                Body::Fixed { tokens } | Body::Dynamic { tokens, .. } => {
                    tokens.iter().map(|t| t.span()).sum()
                }
            };
            let last = g.last;
            blocks.push(g);
            if last {
                break;
            }
        }
        let n = blocks.len();
        if let Body::Dynamic { tokens, .. } = &blocks[0].body {
            let t = tokens.clone();
            blocks.insert(
                0,
                Block {
                    last: false,
                    body: Body::Fixed { tokens: t },
                },
            );
            blocks.remove(1);
        }
        assert_eq!(blocks.len(), n);
        let s = stream(blocks);
        let raw = crate::deflate::write(&s, &data).unwrap();
        let (rs, rd) = crate::deflate::parse(&raw).unwrap();
        let f = parse(&raw).unwrap();
        assert!(f.data == rd);
        let mut toks = vec![];
        for b in &rs.blocks {
            if let Body::Fixed { tokens } | Body::Dynamic { tokens, .. } = &b.body {
                toks.extend_from_slice(tokens);
            }
        }
        assert!(f.tokens == toks);
        assert_eq!(f.stream.blocks.len(), rs.blocks.len());
        assert_eq!(
            (f.stream.tail_pad, &f.stream.trailing),
            (rs.tail_pad, &rs.trailing)
        );
        for (a, b) in f.stream.blocks.iter().zip(&rs.blocks) {
            assert_eq!(a.last, b.last);
            match (&a.body, &b.body) {
                (Body::Dynamic { header: x, .. }, Body::Dynamic { header: y, .. }) => {
                    assert_eq!(x, y)
                }
                (x, y) => assert_eq!(x.btype(), y.btype()),
            }
        }
        // Matches of every distance and length copy right.
        let mut v: Vec<u8> = (0..300u32).map(|i| (i * 37 % 251) as u8).collect();
        for d in 1..=40 {
            for len in [3, 15, 16, 17, 33, 258] {
                let mut w = v.clone();
                w.reserve(len + 16);
                // SAFETY: room reserved, `d <= w.len()`.
                unsafe {
                    copy_match(w.as_mut_ptr(), w.len(), d, len);
                    w.set_len(w.len() + len);
                }
                for _ in 0..len {
                    v.push(v[v.len() - d]);
                }
                assert!(w == v, "d={d} len={len}");
            }
        }
        // Truncations fail rather than panic.
        for cut in [0, 1, 2, raw.len() / 3, raw.len() - 3] {
            let _ = parse(&raw[..cut]);
        }
    }
}
