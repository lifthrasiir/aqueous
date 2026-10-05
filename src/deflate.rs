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

impl Token {
    /// Number of bytes this token produces.
    pub fn span(self) -> usize {
        match self {
            Token::Lit(_) => 1,
            Token::Match { len, .. } => len as usize,
        }
    }
}

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
    pub ops: Vec<RleOp>,
    /// `hlit + hdist` code lengths, literal/length first.
    pub lens: Vec<u8>,
}

impl DynHeader {
    pub fn lit_lens(&self) -> &[u8] {
        &self.lens[..self.hlit as usize]
    }
    pub fn dist_lens(&self) -> &[u8] {
        &self.lens[self.hlit as usize..]
    }
}

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
    let mut ops = Vec::new();
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
    let mut w = BitWriter::new();
    let mut pos = 0usize;
    for b in &stream.blocks {
        w.bits(1, b.last as u32);
        w.bits(2, b.body.btype() as u32);
        match &b.body {
            Body::Stored { pad, len } => {
                w.bits(w.pad_len(), *pad as u32);
                w.bits(16, *len as u32);
                w.bits(16, !*len as u32);
                let end = pos + *len as usize;
                w.bytes(
                    data.get(pos..end)
                        .ok_or(Error::Invalid("stored block past end"))?,
                );
                pos = end;
            }
            Body::Fixed { tokens } => {
                let ll = huffman::fixed_lit_lens();
                write_tokens(&mut w, tokens, &ll, &huffman::FIXED_DIST_LENS, &mut pos)?;
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
                write_tokens(&mut w, tokens, h.lit_lens(), h.dist_lens(), &mut pos)?;
            }
        }
    }
    w.bits(w.pad_len(), stream.tail_pad as u32);
    let mut out = w.finish();
    out.extend_from_slice(&stream.trailing);
    Ok(out)
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
