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
    pub data: Vec<u8>,
    pub tokens: Vec<Token>,
    /// Per block, its first token (or where it would be, for stored blocks), then the end.
    pub starts: Vec<usize>,
    /// Stored blocks as (token index before which they come, bytes).
    pub skips: Vec<(usize, usize)>,
    pub stats: Stats,
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
    next: usize,
    /// No cut before this token index (where stored blocks were).
    no_cut: usize,
}

impl Stats {
    #[inline(always)]
    fn token_at(&mut self, k: usize, p: usize) {
        if p >= self.next && k > 0 && k != self.no_cut {
            self.cuts.push(k);
            self.cut_pos.push(p);
            self.next = p + self.segment;
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
        for (i, &l) in longest[..if max > p { psize } else { 0 }].iter().enumerate() {
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
}

/// Collects the data and tokens (for [`parse_segments`]). The vectors are kept as raw
/// parts and grown by value, so that their lengths can stay in registers in the token loop
/// (a `&mut Vec` would pin them to memory).
struct Collect {
    data: (*mut u8, usize, usize),
    tokens: (*mut Token, usize, usize),
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

impl Collect {
    fn new(data: usize, tokens: usize) -> Collect {
        let mut d = std::mem::ManuallyDrop::new(Vec::<u8>::with_capacity(data));
        let mut t = std::mem::ManuallyDrop::new(Vec::<Token>::with_capacity(tokens));
        Collect {
            data: (d.as_mut_ptr(), 0, d.capacity()),
            tokens: (t.as_mut_ptr(), 0, t.capacity()),
        }
    }

    fn into_vecs(self) -> (Vec<u8>, Vec<Token>) {
        // SAFETY: as in `grow`.
        unsafe {
            (
                Vec::from_raw_parts(self.data.0, self.data.1, self.data.2),
                Vec::from_raw_parts(self.tokens.0, self.tokens.1, self.tokens.2),
            )
        }
    }
}

impl Sink for Collect {
    #[inline(always)]
    fn reserve(&mut self) {
        if self.data.2 - self.data.1 < ROOM {
            self.data = grow(self.data, ROOM);
        }
        if self.tokens.2 - self.tokens.1 < 3 {
            self.tokens = grow(self.tokens, 3);
        }
    }

    #[inline(always)]
    fn lit(&mut self, b: u8) {
        // SAFETY: `reserve` came before, and leaves room for three literals.
        unsafe {
            *self.data.0.add(self.data.1) = b;
            self.tokens.0.add(self.tokens.1).write(Token::Lit(b));
        }
        self.data.1 += 1;
        self.tokens.1 += 1;
    }

    #[inline(always)]
    fn mat(&mut self, len: u16, dist: u16, alt258: bool) {
        // SAFETY: `1 <= dist <=` the bytes so far (checked by the parser), and `reserve`
        // left room for the match and its overshoot.
        unsafe {
            copy_match(self.data.0, self.data.1, dist as usize, len as usize);
            self.tokens.0.add(self.tokens.1).write(Token::Match { len, dist, alt258 });
        }
        self.data.1 += len as usize;
        self.tokens.1 += 1;
    }

    fn stored(&mut self, bytes: &[u8]) {
        if self.data.2 - self.data.1 < bytes.len() + ROOM {
            self.data = grow(self.data, bytes.len() + ROOM);
        }
        // SAFETY: room made just above.
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), self.data.0.add(self.data.1), bytes.len()) };
        self.data.1 += bytes.len();
    }
}

/// Parses `raw` (see the module documentation).
pub fn parse(raw: &[u8]) -> Result<Flat> {
    parse_segments(raw, usize::MAX)
}

/// Parses `raw`, with segments of about `segment` bytes in [`Stats`].
pub fn parse_segments(raw: &[u8], segment: usize) -> Result<Flat> {
    let mut c = Collect::new(raw.len() * 3, raw.len());
    let l = parse_into(raw, segment, &mut c);
    let (data, tokens) = c.into_vecs();
    let l = l?;
    Ok(Flat {
        stream: l.stream,
        data,
        tokens,
        starts: l.starts,
        skips: l.skips,
        stats: l.stats,
    })
}

/// Parses `raw` like [`parse_segments`], handing the data and tokens to `sink`.
pub fn parse_into<S: Sink>(raw: &[u8], segment: usize, sink: &mut S) -> Result<Layout> {
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
        next: segment,
        no_cut: usize::MAX,
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
                let bytes = raw
                    .get(at..at + len as usize)
                    .ok_or(Error::Truncated)?;
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
                tokens_into(&mut b, &fixed.0, &fixed.1, sink, &mut olen, &mut tlen, &mut st)?;
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
    let trailing = raw[b.bit_pos() / 8..].to_vec();
    Ok(Layout {
        stream: Stream {
            blocks,
            tail_pad,
            trailing,
        },
        starts,
        skips,
        stats: st,
        len: olen,
    })
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
            break b.check().and(Err(Error::Invalid("distance before start of data")));
        }
        max_dist = max_dist.max(d as u16);
        // Length codes 0 and 1 are lengths 3 and 4; distance codes 24 and 26 start past
        // 4096 and 8192.
        far |= (c == 0 && dc >= 24) as u8 | ((c == 0 && dc >= 26) as u8) << 1 | ((c <= 1 && dc >= 24) as u8) << 2;
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
unsafe fn copy_match(base: *mut u8, n: usize, d: usize, len: usize) {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Gives what `deflate::parse` gives, on streams of every block type.
    #[test]
    fn same_as_reference_parser() {
        let mut lcg = 5u32;
        let mut data = include_bytes!("../predict.rs").to_vec();
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
        while let Some(g) = crate::predict::zlib_block(&mut e, pos) {
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
            blocks.insert(0, Block { last: false, body: Body::Fixed { tokens: t } });
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
        assert_eq!((f.stream.tail_pad, &f.stream.trailing), (rs.tail_pad, &rs.trailing));
        for (a, b) in f.stream.blocks.iter().zip(&rs.blocks) {
            assert_eq!(a.last, b.last);
            match (&a.body, &b.body) {
                (Body::Dynamic { header: x, .. }, Body::Dynamic { header: y, .. }) => assert_eq!(x, y),
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
