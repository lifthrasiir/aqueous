//! A fast parser of raw DEFLATE streams into the decompressed data, one flat list of tokens
//! and the blocks' structure. It gives exactly what [`crate::deflate::parse`] gives (with
//! the tokens moved out of the blocks), several times faster: a 64-bit bit buffer and
//! single-level decoding tables. Statistics the rest needs of the tokens are gathered on
//! the way, sparing passes over them.

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

/// A decoding table: entry = symbol | code length << 16 (0: no code).
struct Table {
    t: Vec<u32>,
    mask: u64,
}

impl Table {
    fn new(lens: &[u8]) -> Result<Table> {
        let mut t = Table { t: vec![], mask: 0 };
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
        let max = (1..16).rev().find(|&l| count[l] > 0).unwrap_or(1) as u32;
        let size = 1usize << max;
        // A complete code fills every entry; otherwise the rest must read as no code.
        let kraft: u64 = (1..16).map(|l| (count[l] as u64) << (15 - l)).sum();
        if kraft != 1 << 15 {
            self.t.clear();
        }
        self.t.resize(size, 0);
        let mut next = [0u32; 16];
        let mut code = 0;
        for l in 1..16 {
            code = (code + count[l - 1]) << 1;
            next[l] = code;
        }
        for (sym, &l) in lens.iter().enumerate() {
            if l == 0 {
                continue;
            }
            let c = next[l as usize];
            next[l as usize] += 1;
            let e = sym as u32 | (l as u32) << 16;
            let mut i = (c.reverse_bits() >> (32 - l as u32)) as usize;
            while i < size {
                self.t[i] = e;
                i += 1 << l;
            }
        }
        self.mask = size as u64 - 1;
        Ok(())
    }

    #[inline(always)]
    fn decode(&self, b: &mut Bits) -> Result<u32> {
        let e = self.t[(b.buf & self.mask) as usize];
        let l = e >> 16;
        if l == 0 {
            return Err(Error::Invalid("undecodable Huffman code"));
        }
        b.bits(l);
        Ok(e & 0xffff)
    }
}

/// Parses `raw` (see the module documentation).
pub fn parse(raw: &[u8]) -> Result<Flat> {
    parse_segments(raw, usize::MAX)
}

/// Parses `raw`, with segments of about `segment` bytes in [`Stats`].
pub fn parse_segments(raw: &[u8], segment: usize) -> Result<Flat> {
    let mut b = Bits {
        d: raw,
        pos: 0,
        buf: 0,
        n: 0,
    };
    let mut data = Vec::with_capacity(raw.len() * 3);
    let mut tokens = Vec::with_capacity(raw.len());
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
    let fixed = (
        Table::new(&huffman::fixed_lit_lens())?,
        Table::new(&huffman::FIXED_DIST_LENS)?,
    );
    let (mut lit, mut dist) = (Table::new(&[1])?, Table::new(&[1])?);
    loop {
        let last = b.get(1) == 1;
        starts.push(tokens.len());
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
                data.extend_from_slice(bytes);
                b = Bits {
                    d: raw,
                    pos: at + len as usize,
                    buf: 0,
                    n: 0,
                };
                skips.push((tokens.len(), len as usize));
                st.no_cut = tokens.len();
                st.freqs.push(([0; 288], [0; 32]));
                Body::Stored { pad, len }
            }
            1 => {
                tokens_into(&mut b, &fixed.0, &fixed.1, &mut data, &mut tokens, &mut st)?;
                Body::Fixed { tokens: vec![] }
            }
            2 => {
                let header = parse_header(&mut b)?;
                lit.build(header.lit_lens())?;
                dist.build(header.dist_lens())?;
                tokens_into(&mut b, &lit, &dist, &mut data, &mut tokens, &mut st)?;
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
    starts.push(tokens.len());
    st.cuts.push(tokens.len());
    let k = (b.bit_pos() as u32).wrapping_neg() & 7;
    let tail_pad = b.get(k) as u8;
    b.check()?;
    let trailing = raw[b.bit_pos() / 8..].to_vec();
    Ok(Flat {
        stream: Stream {
            blocks,
            tail_pad,
            trailing,
        },
        data,
        tokens,
        starts,
        skips,
        stats: st,
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
    let cl = Table::new(&cl_lens)?;
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

fn tokens_into(
    bits: &mut Bits,
    lit: &Table,
    dist: &Table,
    out: &mut Vec<u8>,
    tokens: &mut Vec<Token>,
    st: &mut Stats,
) -> Result<()> {
    // Room the loop needs per token: a match and the overshoot of its copy.
    const ROOM: usize = MAX_MATCH + 16;
    let mut b = *bits;
    let (mut lf, mut df) = ([0u32; 288], [0u32; 32]);
    let (mut max_dist, mut far) = (st.max_dist, st.far);
    // The vectors' lengths, and pointers valid while their capacity covers `ROOM` more
    // (bytes) and three more (tokens).
    let (mut olen, mut tlen) = (out.len(), tokens.len());
    let (mut optr, mut tptr) = (out.as_mut_ptr(), tokens.as_mut_ptr());
    let (mut ocap, mut tcap) = (out.capacity(), tokens.capacity());
    let r = loop {
        if ocap - olen < ROOM || tcap - tlen < 3 {
            // SAFETY: everything up to the lengths is written.
            unsafe {
                out.set_len(olen);
                tokens.set_len(tlen);
            }
            out.reserve(ROOM.max(olen / 2));
            tokens.reserve(3.max(tlen / 2));
            (optr, tptr) = (out.as_mut_ptr(), tokens.as_mut_ptr());
            (ocap, tcap) = (out.capacity(), tokens.capacity());
        }
        // A token takes at most 15 + 5 + 15 + 13 = 48 bits.
        b.refill();
        if b.pos > b.d.len() + 16 {
            break Err(Error::Truncated);
        }
        if olen >= st.next {
            st.token_at(tlen, olen);
        }
        let sym = match lit.decode(&mut b) {
            Ok(s) => s,
            Err(e) => break Err(e),
        };
        lf[sym as usize] += 1;
        if sym < 256 {
            // SAFETY: within capacity (checked above).
            unsafe {
                *optr.add(olen) = sym as u8;
                tptr.add(tlen).write(Token::Lit(sym as u8));
            }
            olen += 1;
            tlen += 1;
            // Two more literals fit in the bits left (at least 56 - 15), unless a segment
            // may start before them.
            if olen + 2 < st.next {
                for _ in 0..2 {
                    let e = lit.t[(b.buf & lit.mask) as usize];
                    let sym = e & 0xffff;
                    if sym >= 256 || e >> 16 == 0 {
                        break;
                    }
                    b.bits(e >> 16);
                    lf[sym as usize] += 1;
                    // SAFETY: within capacity (room for three tokens checked above).
                    unsafe {
                        *optr.add(olen) = sym as u8;
                        tptr.add(tlen).write(Token::Lit(sym as u8));
                    }
                    olen += 1;
                    tlen += 1;
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
        if d > olen {
            break b.check().and(Err(Error::Invalid("distance before start of data")));
        }
        max_dist = max_dist.max(d as u16);
        // Length codes 0 and 1 are lengths 3 and 4; distance codes 24 and 26 start past
        // 4096 and 8192.
        far |= (c == 0 && dc >= 24) as u8 | ((c == 0 && dc >= 26) as u8) << 1 | ((c <= 1 && dc >= 24) as u8) << 2;
        // SAFETY: `1 <= d <= olen`, and the capacity covers `ROOM` more bytes.
        unsafe {
            copy_match(optr, olen, d, len as usize);
            tptr.add(tlen).write(Token::Match {
                len,
                dist: d as u16,
                alt258,
            });
        }
        olen += len as usize;
        tlen += 1;
    };
    // SAFETY: as above.
    unsafe {
        out.set_len(olen);
        tokens.set_len(tlen);
    }
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
    // Every chunk read lies before the chunk written (16 bytes with `d >= 16`, 8 with
    // `d >= 8`) and within what is initialized: `src + i + chunk <= dst + i`. A distance of 1
    // repeats a byte; other short ones go byte by byte, each read before the write that may
    // need it.
    unsafe {
        let dst = base.add(n);
        let src = base.add(n - d);
        if d >= 16 {
            let mut i = 0;
            while i < len {
                std::ptr::copy_nonoverlapping(src.add(i), dst.add(i), 16);
                i += 16;
            }
        } else if d >= 8 {
            let mut i = 0;
            while i < len {
                std::ptr::copy_nonoverlapping(src.add(i), dst.add(i), 8);
                i += 8;
            }
        } else if d == 1 {
            std::ptr::write_bytes(dst, *src, len);
        } else {
            for i in 0..len {
                *dst.add(i) = *src.add(i);
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
