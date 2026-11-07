//! A fully explicit representation of a raw DEFLATE stream, with a parser and a bit-exact
//! writer. Every degree of freedom an encoder has is a field here; everything else is
//! derived from the decompressed data.

use crate::bits::{BitReader, BitWriter};
use crate::huffman::{self, CL_ORDER, DIST_BASE, DIST_EXTRA, LEN_BASE, LEN_EXTRA, RleOp};
use crate::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Token {
    Lit(u8),
    /// `alt258` marks length 258 sent as symbol 284 with extra bits 31 instead of 285.
    Match {
        len: u16,
        dist: u16,
        alt258: bool,
    },
}

impl aqr_common::wire::Wire for Token {
    fn put(&self, out: &mut Vec<u8>) {
        match *self {
            Token::Lit(b) => out.extend_from_slice(&[0, b]),
            Token::Match { len, dist, alt258 } => {
                out.push(1 + alt258 as u8);
                len.put(out);
                dist.put(out);
            }
        }
    }

    fn size(&self) -> usize {
        match self {
            Token::Lit(_) => 2,
            Token::Match { .. } => 5,
        }
    }

    fn get(b: &mut &[u8]) -> Option<Self> {
        Some(match u8::get(b)? {
            0 => Token::Lit(u8::get(b)?),
            t @ (1 | 2) => Token::Match {
                len: u16::get(b)?,
                dist: u16::get(b)?,
                alt258: t == 2,
            },
            _ => return None,
        })
    }
}

/// A token in 5 bytes (as tasks give them back): its kind, then its byte, or its length and
/// distance.
pub fn token_bytes(t: Token) -> [u8; 5] {
    match t {
        Token::Lit(b) => [0, b, 0, 0, 0],
        Token::Match { len, dist, alt258 } => {
            let (l, d) = (len.to_le_bytes(), dist.to_le_bytes());
            [1 + alt258 as u8, l[0], l[1], d[0], d[1]]
        }
    }
}

/// The token of [`token_bytes`].
pub fn token_of(b: [u8; 5]) -> Option<Token> {
    Some(match b[0] {
        0 => Token::Lit(b[1]),
        t @ (1 | 2) => Token::Match {
            len: u16::from_le_bytes([b[1], b[2]]),
            dist: u16::from_le_bytes([b[3], b[4]]),
            alt258: t == 2,
        },
        _ => return None,
    })
}

impl Token {
    /// Number of bytes this token produces.
    pub fn span(self) -> usize {
        match self {
            Token::Lit(_) => 1,
            Token::Match { len, .. } => len as usize,
        }
    }
}

/// Up to `N` values in place (no allocation): what a block's header holds and what its
/// candidates are made of, so that headers are read and kept without the heap.
#[derive(Clone, Copy)]
pub struct Short<T: Copy + Default, const N: usize> {
    v: [T; N],
    n: u16,
}

impl<T: Copy + Default, const N: usize> Short<T, N> {
    pub fn new() -> Self {
        Short {
            v: [T::default(); N],
            n: 0,
        }
    }

    /// The first `n` values of `T::default()` (as `vec![x; n]` of the default).
    pub fn zeroed(n: usize) -> Self {
        assert!(n <= N, "{n} values past {N}");
        Short {
            v: [T::default(); N],
            n: n as u16,
        }
    }

    pub fn from_slice(s: &[T]) -> Self {
        let mut x = Self::new();
        x.extend_from_slice(s);
        x
    }

    #[inline]
    pub fn push(&mut self, t: T) {
        assert!((self.n as usize) < N, "{N} values at most");
        self.v[self.n as usize] = t;
        self.n += 1;
    }

    pub fn extend_from_slice(&mut self, s: &[T]) {
        let n = self.n as usize;
        assert!(n + s.len() <= N, "{N} values at most");
        self.v[n..n + s.len()].copy_from_slice(s);
        self.n += s.len() as u16;
    }

    pub fn resize(&mut self, n: usize, t: T) {
        assert!(n <= N, "{N} values at most");
        let m = self.n as usize;
        if n > m {
            self.v[m..n].fill(t);
        }
        self.n = n as u16;
    }

    pub fn truncate(&mut self, n: usize) {
        self.n = self.n.min(n as u16);
    }

    pub fn clear(&mut self) {
        self.n = 0;
    }
}

impl<T: Copy + Default, const N: usize> Default for Short<T, N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: Copy + Default, const N: usize> std::ops::Deref for Short<T, N> {
    type Target = [T];
    fn deref(&self) -> &[T] {
        &self.v[..self.n as usize]
    }
}

impl<T: Copy + Default, const N: usize> std::ops::DerefMut for Short<T, N> {
    fn deref_mut(&mut self) -> &mut [T] {
        &mut self.v[..self.n as usize]
    }
}

impl<T: Copy + Default + PartialEq, const N: usize> PartialEq for Short<T, N> {
    fn eq(&self, o: &Self) -> bool {
        **self == **o
    }
}

impl<T: Copy + Default + Eq, const N: usize> Eq for Short<T, N> {}

impl<T: Copy + Default + std::fmt::Debug, const N: usize> std::fmt::Debug for Short<T, N> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        (**self).fmt(f)
    }
}

impl<T: Copy + Default, const N: usize> Extend<T> for Short<T, N> {
    fn extend<I: IntoIterator<Item = T>>(&mut self, it: I) {
        it.into_iter().for_each(|t| self.push(t));
    }
}

impl<T: Copy + Default, const N: usize> FromIterator<T> for Short<T, N> {
    fn from_iter<I: IntoIterator<Item = T>>(it: I) -> Self {
        let mut x = Self::new();
        x.extend(it);
        x
    }
}

impl<'a, T: Copy + Default, const N: usize> IntoIterator for &'a Short<T, N> {
    type Item = &'a T;
    type IntoIter = std::slice::Iter<'a, T>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

/// Code lengths or run-length ops of a header: `hlit + hdist` (320) at most.
pub const HEADER_LENS: usize = 320;
pub type Lens = Short<u8, HEADER_LENS>;
pub type Ops = Short<RleOp, HEADER_LENS>;

/// Dynamic Huffman block header, exactly as transmitted.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DynHeader {
    /// Number of literal/length code lengths (257..=288).
    pub hlit: u16,
    /// Number of distance code lengths (1..=32).
    pub hdist: u8,
    /// Number of code-length code lengths sent (4..=19).
    pub hclen: u8,
    /// Code-length code lengths indexed by symbol (not transmission order). Entries past
    /// `hclen` in transmission order are zero.
    pub cl_lens: [u8; 19],
    /// The run-length coded sequence that expands to `lens`.
    pub ops: Ops,
    /// `hlit + hdist` code lengths, literal/length first.
    pub lens: Lens,
}

impl DynHeader {
    pub fn lit_lens(&self) -> &[u8] {
        &self.lens[..self.hlit as usize]
    }
    pub fn dist_lens(&self) -> &[u8] {
        &self.lens[self.hlit as usize..]
    }
}

// (a dynamic block's header in place: blocks are read and written without the heap)
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Body {
    Stored {
        /// Value of the bits skipped to reach the byte boundary (normally zero).
        pad: u8,
        len: u16,
    },
    Fixed {
        tokens: Vec<Token>,
    },
    Dynamic {
        header: DynHeader,
        tokens: Vec<Token>,
    },
}

impl Body {
    pub fn btype(&self) -> u8 {
        match self {
            Body::Stored { .. } => 0,
            Body::Fixed { .. } => 1,
            Body::Dynamic { .. } => 2,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    pub last: bool,
    pub body: Body,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Stream {
    pub blocks: Vec<Block>,
    /// Value of the bits after the final block up to the byte boundary (normally zero).
    pub tail_pad: u8,
    /// Bytes after the end of the DEFLATE stream.
    pub trailing: Vec<u8>,
}

/// Parses a raw DEFLATE stream, returning its structure and the decompressed data.
pub fn parse(raw: &[u8]) -> Result<(Stream, Vec<u8>)> {
    let mut r = BitReader::new(raw);
    let mut out = Vec::new();
    let mut blocks = Vec::new();
    loop {
        let last = r.bits(1)? == 1;
        let body = match r.bits(2)? {
            0 => {
                let pad = r.bits(r.pad_len())? as u8;
                let len = r.bits(16)? as u16;
                let nlen = r.bits(16)? as u16;
                if nlen != !len {
                    return Err(Error::Invalid("stored block length mismatch"));
                }
                out.extend_from_slice(r.bytes(len as usize)?);
                Body::Stored { pad, len }
            }
            1 => {
                let lit = huffman::Decoder::new(&huffman::fixed_lit_lens())?;
                let dist = huffman::Decoder::new(&huffman::FIXED_DIST_LENS)?;
                Body::Fixed {
                    tokens: parse_tokens(&mut r, &lit, &dist, &mut out)?,
                }
            }
            2 => {
                let header = parse_header(&mut r)?;
                let lit = huffman::Decoder::new(header.lit_lens())?;
                let dist = huffman::Decoder::new(header.dist_lens())?;
                let tokens = parse_tokens(&mut r, &lit, &dist, &mut out)?;
                Body::Dynamic { header, tokens }
            }
            _ => return Err(Error::Invalid("reserved block type")),
        };
        blocks.push(Block { last, body });
        if last {
            break;
        }
    }
    let tail_pad = r.bits(r.pad_len())? as u8;
    let trailing = raw[r.bit_pos() / 8..].to_vec();
    Ok((
        Stream {
            blocks,
            tail_pad,
            trailing,
        },
        out,
    ))
}

fn parse_header(r: &mut BitReader) -> Result<DynHeader> {
    let hlit = r.bits(5)? as u16 + 257;
    let hdist = r.bits(5)? as u8 + 1;
    let hclen = r.bits(4)? as u8 + 4;
    let mut cl_lens = [0u8; 19];
    for &s in &CL_ORDER[..hclen as usize] {
        cl_lens[s] = r.bits(3)? as u8;
    }
    let cl = huffman::Decoder::new(&cl_lens)?;
    let total = hlit as usize + hdist as usize;
    let mut ops = Ops::new();
    let mut n = 0;
    while n < total {
        let sym = cl.decode(r)? as u8;
        let extra = r.bits(RleOp::extra_bits(sym))? as u8;
        let op = RleOp { sym, extra };
        n += op.count();
        ops.push(op);
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

fn parse_tokens(
    r: &mut BitReader,
    lit: &huffman::Decoder,
    dist: &huffman::Decoder,
    out: &mut Vec<u8>,
) -> Result<Vec<Token>> {
    let mut tokens = Vec::new();
    loop {
        let sym = lit.decode(r)?;
        match sym {
            0..=255 => {
                out.push(sym as u8);
                tokens.push(Token::Lit(sym as u8));
            }
            256 => return Ok(tokens),
            257..=285 => {
                let c = sym as usize - 257;
                let e = r.bits(LEN_EXTRA[c] as u32)? as u16;
                let len = LEN_BASE[c] + e;
                let alt258 = c == 27 && e == 31;
                let dc = dist.decode(r)? as usize;
                if dc >= 30 {
                    return Err(Error::Invalid("invalid distance symbol"));
                }
                let d = DIST_BASE[dc] + r.bits(DIST_EXTRA[dc] as u32)? as u16;
                if d as usize > out.len() {
                    return Err(Error::Invalid("distance before start of data"));
                }
                let start = out.len() - d as usize;
                for i in 0..len as usize {
                    out.push(out[start + i]);
                }
                tokens.push(Token::Match {
                    len,
                    dist: d,
                    alt258,
                });
            }
            _ => return Err(Error::Invalid("invalid literal/length symbol")),
        }
    }
}

/// Serializes `stream`; `data` supplies the contents of stored blocks.
pub fn write(stream: &Stream, data: &[u8]) -> Result<Vec<u8>> {
    write_threads(stream, data, 1)
}

/// Long streams are written in parallel chunks of at least this many bytes of data.
const MIN_CHUNK: usize = 1 << 20;

/// Like [`write()`], on up to `threads` threads: the blocks are split in chunks that are
/// written separately and joined bit by bit. Chunks with stored blocks, whose padding
/// depends on where they start, are written in place instead.
pub fn write_threads(stream: &Stream, data: &[u8], threads: usize) -> Result<Vec<u8>> {
    let blocks = &stream.blocks;
    let mut starts = Vec::with_capacity(blocks.len() + 1);
    let mut pos = 0usize;
    for b in blocks {
        starts.push(pos);
        pos += match &b.body {
            Body::Stored { len, .. } => *len as usize,
            Body::Fixed { tokens } | Body::Dynamic { tokens, .. } => {
                tokens.iter().map(|t| t.span()).sum()
            }
        };
    }
    starts.push(pos);
    // Chunks of about equal data, at block boundaries.
    let nchunks = threads.min(pos / MIN_CHUNK).max(1);
    let mut cuts = vec![0];
    for i in 1..nchunks {
        let b = starts.partition_point(|&p| p < pos * i / nchunks);
        if b > *cuts.last().unwrap() && b < blocks.len() {
            cuts.push(b);
        }
    }
    cuts.push(blocks.len());
    let chunks: Vec<(usize, usize)> = cuts.windows(2).map(|c| (c[0], c[1])).collect();
    let stored = |&(a, b): &(usize, usize)| {
        blocks[a..b]
            .iter()
            .any(|b| matches!(b.body, Body::Stored { .. }))
    };
    let parallel = chunks.len() > 1;
    let apart: Vec<usize> = (0..chunks.len())
        .filter(|&i| parallel && !stored(&chunks[i]))
        .collect();
    let mut written = aqr_common::pool::par_map(apart.len(), threads, |k| -> Result<_> {
        let c = chunks[apart[k]];
        let mut w = BitWriter::new();
        let mut pos = starts[c.0];
        write_blocks(&mut w, &blocks[c.0..c.1], data, &mut pos)?;
        Ok(w.into_parts())
    })
    .into_iter();
    let parts: Vec<_> = (0..chunks.len())
        .map(|i| apart.contains(&i).then(|| written.next().unwrap()))
        .collect();
    let mut w = BitWriter::new();
    for (c, part) in chunks.iter().zip(parts) {
        match part {
            Some(p) => {
                let (out, acc, nacc) = p?;
                w.append(&out, acc, nacc);
            }
            None => {
                let mut pos = starts[c.0];
                write_blocks(&mut w, &blocks[c.0..c.1], data, &mut pos)?;
            }
        }
    }
    w.bits(w.pad_len(), stream.tail_pad as u32);
    let mut out = w.finish();
    out.extend_from_slice(&stream.trailing);
    Ok(out)
}

/// Writes `blocks`, the first of which starts at `pos` in `data`.
fn write_blocks(w: &mut BitWriter, blocks: &[Block], data: &[u8], pos: &mut usize) -> Result<()> {
    for b in blocks {
        w.bits(1, b.last as u32);
        w.bits(2, b.body.btype() as u32);
        match &b.body {
            Body::Stored { pad, len } => {
                w.bits(w.pad_len(), *pad as u32);
                w.bits(16, *len as u32);
                w.bits(16, !*len as u32);
                let end = *pos + *len as usize;
                w.bytes(
                    data.get(*pos..end)
                        .ok_or(Error::Invalid("stored block past end"))?,
                );
                *pos = end;
            }
            Body::Fixed { tokens } => {
                let ll = huffman::fixed_lit_lens();
                write_tokens(w, tokens, &ll, &huffman::FIXED_DIST_LENS, pos)?;
            }
            Body::Dynamic { header: h, tokens } => {
                w.bits(5, h.hlit as u32 - 257);
                w.bits(5, h.hdist as u32 - 1);
                w.bits(4, h.hclen as u32 - 4);
                for &s in &CL_ORDER[..h.hclen as usize] {
                    w.bits(3, h.cl_lens[s] as u32);
                }
                let clc = huffman::codes(&h.cl_lens);
                for op in &h.ops {
                    let l = h.cl_lens[op.sym as usize];
                    if l == 0 {
                        return Err(Error::Invalid("code-length symbol without a code"));
                    }
                    w.bits(l as u32, clc[op.sym as usize] as u32);
                    w.bits(RleOp::extra_bits(op.sym), op.extra as u32);
                }
                write_tokens(w, tokens, h.lit_lens(), h.dist_lens(), pos)?;
            }
        }
    }
    Ok(())
}

fn write_tokens(
    w: &mut BitWriter,
    tokens: &[Token],
    ll: &[u8],
    dl: &[u8],
    pos: &mut usize,
) -> Result<()> {
    let lc = huffman::codes(ll);
    let dc = huffman::codes(dl);
    let put = |w: &mut BitWriter, lens: &[u8], codes: &[u16], sym: usize| match lens.get(sym) {
        Some(&l) if l > 0 => {
            w.bits(l as u32, codes[sym] as u32);
            Ok(())
        }
        _ => Err(Error::Invalid("symbol without a code")),
    };
    for &t in tokens {
        match t {
            Token::Lit(b) => put(w, ll, &lc, b as usize)?,
            Token::Match { len, dist, alt258 } => {
                let c = if alt258 { 27 } else { huffman::len_code(len) };
                put(w, ll, &lc, 257 + c)?;
                w.bits(LEN_EXTRA[c] as u32, (len - LEN_BASE[c]) as u32);
                let d = huffman::dist_code(dist);
                put(w, dl, &dc, d)?;
                w.bits(DIST_EXTRA[d] as u32, (dist - DIST_BASE[d]) as u32);
            }
        }
        *pos += t.span();
    }
    put(w, ll, &lc, 256)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Writing in parallel chunks gives the same bits, stored blocks (whose padding depends on
    /// the bit position) included.
    #[test]
    fn parallel_write_is_identical() {
        let mut lcg = 5u32;
        let mut rnd = |n: u32| {
            lcg = lcg.wrapping_mul(1_103_515_245).wrapping_add(12345);
            (lcg >> 8) % n
        };
        let mut data = vec![];
        let mut blocks = vec![];
        while data.len() < 3 << 20 {
            // Stored blocks only in the first chunk, so that the others are written apart.
            let body = match rnd(4) {
                0 if data.len() < 1 << 20 => {
                    let len = rnd(3000) as u16;
                    data.extend((0..len).map(|_| rnd(256) as u8));
                    Body::Stored { pad: 0, len }
                }
                _ => {
                    let n = 1 + rnd(40_000) as usize;
                    let tokens: Vec<Token> = (0..n)
                        .map(|_| {
                            if data.len() > 300 && rnd(4) == 0 {
                                let (len, dist) = (3 + rnd(256) as u16, 1 + rnd(300) as u16);
                                for _ in 0..len {
                                    data.push(data[data.len() - dist as usize]);
                                }
                                Token::Match {
                                    len,
                                    dist,
                                    alt258: false,
                                }
                            } else {
                                let b = rnd(256) as u8;
                                data.push(b);
                                Token::Lit(b)
                            }
                        })
                        .collect();
                    Body::Fixed { tokens }
                }
            };
            blocks.push(Block { last: false, body });
        }
        blocks.last_mut().unwrap().last = true;
        let stream = Stream {
            blocks,
            tail_pad: 0,
            trailing: vec![1, 2, 3],
        };
        let one = write(&stream, &data).unwrap();
        assert_eq!(parse(&one).unwrap(), (stream.clone(), data.clone()));
        for threads in 2..=9 {
            assert!(
                write_threads(&stream, &data, threads).unwrap() == one,
                "{threads} threads"
            );
        }
    }
}
