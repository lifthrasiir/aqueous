//! A fast writer of DEFLATE streams from the blocks' layout and one flat list of tokens,
//! giving exactly what [`crate::deflate::write`] gives: code lookups by table rather than by
//! search, and a 64-bit bit buffer.

use crate::deflate::{Block, Body, Stream, Token};
use crate::huffman::{self, CL_ORDER, DIST_BASE, DIST_EXTRA, LEN_BASE, LEN_EXTRA, RleOp};
use crate::{Error, Result};

struct Bits {
    out: Vec<u8>,
    acc: u64,
    n: u32,
}

impl Bits {
    /// Appends `k <= 32` bits of `v`.
    #[inline(always)]
    fn put(&mut self, k: u32, v: u32) {
        self.acc |= (v as u64) << self.n;
        self.n += k;
        if self.n >= 32 {
            self.out.extend_from_slice(&(self.acc as u32).to_le_bytes());
            self.acc >>= 32;
            self.n -= 32;
        }
    }

    fn pad_len(&self) -> u32 {
        (8 - self.n % 8) % 8
    }

    /// Writes out the whole bytes pending (at a byte boundary).
    fn flush(&mut self) {
        while self.n >= 8 {
            self.out.push(self.acc as u8);
            self.acc >>= 8;
            self.n -= 8;
        }
    }
}

/// Length code (0-based) of every match length.
pub(super) static LEN_CODE: [u8; 259] = {
    let mut t = [0u8; 259];
    let mut c = 0;
    let mut l = 3;
    while l <= 258 {
        while c + 1 < 29 && LEN_BASE[c + 1] as usize <= l {
            c += 1;
        }
        t[l] = c as u8;
        l += 1;
    }
    t
};

/// Distance code of distances 1..=256, and of the larger ones by `(dist - 1) >> 7`.
static DIST_LO: [u8; 256] = {
    let mut t = [0u8; 256];
    let mut c = 0;
    let mut d = 1;
    while d <= 256 {
        while c + 1 < 30 && DIST_BASE[c + 1] as usize <= d {
            c += 1;
        }
        t[d - 1] = c as u8;
        d += 1;
    }
    t
};
static DIST_HI: [u8; 256] = {
    let mut t = [0u8; 256];
    let mut i = 2;
    while i < 256 {
        // The code of distance i * 128 + 1 (codes are constant over such spans past 256).
        let d = i * 128 + 1;
        let mut c = 0;
        while c + 1 < 30 && DIST_BASE[c + 1] as usize <= d {
            c += 1;
        }
        t[i] = c as u8;
        i += 1;
    }
    t
};

#[inline(always)]
pub(super) fn dist_code(d: usize) -> usize {
    if d <= 256 {
        DIST_LO[d - 1] as usize
    } else {
        DIST_HI[(d - 1) >> 7] as usize
    }
}

/// (code, length) per symbol; length 0 for symbols without a code.
fn table(lens: &[u8], n: usize) -> Vec<(u32, u32)> {
    let codes = huffman::codes(lens);
    let mut t = vec![(0, 0); n];
    for (i, (&l, &c)) in lens.iter().zip(&codes).enumerate().take(n) {
        t[i] = (c as u32, l as u32);
    }
    t
}

/// Writes `stream` (whose blocks' tokens are left out) with the tokens of block `i` being
/// `tokens[starts[i]..starts[i + 1]]`; `data` supplies stored blocks.
pub fn write(stream: &Stream, starts: &[usize], tokens: &[Token], data: &[u8]) -> Result<Vec<u8>> {
    let mut w = Writer::new(data.len() / 2 + 64);
    for (i, b) in stream.blocks.iter().enumerate() {
        let toks = tokens
            .get(starts[i]..starts[i + 1])
            .ok_or(Error::Invalid("tokens past the end"))?;
        w.block(b, toks, data)?;
    }
    w.finish(stream.tail_pad, &stream.trailing)
}

/// Writes a stream block after block, its bytes taken as they are written ([`Writer::take`]):
/// what [`write`] does, without having the whole stream at once.
pub struct Writer {
    w: Bits,
    /// Bytes of the data written so far.
    pos: usize,
}

impl Writer {
    /// A writer with room for `capacity` bytes.
    pub fn new(capacity: usize) -> Writer {
        Writer {
            w: Bits {
                out: Vec::with_capacity(capacity),
                acc: 0,
                n: 0,
            },
            pos: 0,
        }
    }

    /// Bytes of the data written so far, where the next block's data starts.
    pub fn pos(&self) -> usize {
        self.pos
    }

    /// Writes block `b` with tokens `toks`, of `data` from [`Writer::pos`] on.
    pub fn block(&mut self, b: &Block, toks: &[Token], data: &[u8]) -> Result<()> {
        static FIXED: std::sync::OnceLock<[Vec<(u32, u32)>; 2]> = std::sync::OnceLock::new();
        let w = &mut self.w;
        w.put(1, b.last as u32);
        w.put(2, b.body.btype() as u32);
        match &b.body {
            Body::Stored { pad, len } => {
                if *pad as u32 >> w.pad_len() != 0 {
                    return Err(Error::Invalid("padding wider than its bits"));
                }
                w.put(w.pad_len(), *pad as u32);
                w.put(16, *len as u32);
                w.put(16, !*len as u32);
                w.flush();
                let end = self.pos + *len as usize;
                w.out.extend_from_slice(
                    data.get(self.pos..end)
                        .ok_or(Error::Invalid("stored block past end"))?,
                );
                self.pos = end;
            }
            Body::Fixed { .. } => {
                let fixed = FIXED.get_or_init(|| {
                    [
                        table(&huffman::fixed_lit_lens(), 288),
                        table(&huffman::FIXED_DIST_LENS, 32),
                    ]
                });
                self.pos += write_tokens(w, toks, &fixed[0], &fixed[1])?;
            }
            Body::Dynamic { header: h, .. } => {
                w.put(5, h.hlit as u32 - 257);
                w.put(5, h.hdist as u32 - 1);
                w.put(4, h.hclen as u32 - 4);
                for &s in &CL_ORDER[..h.hclen as usize] {
                    w.put(3, h.cl_lens[s] as u32);
                }
                let clc = huffman::codes(&h.cl_lens);
                for op in &h.ops {
                    let l = h.cl_lens[op.sym as usize];
                    if l == 0 {
                        return Err(Error::Invalid("code-length symbol without a code"));
                    }
                    w.put(l as u32, clc[op.sym as usize] as u32);
                    w.put(RleOp::extra_bits(op.sym), op.extra as u32);
                }
                let lt = table(h.lit_lens(), 288);
                let dt = table(h.dist_lens(), 32);
                self.pos += write_tokens(w, toks, &lt, &dt)?;
            }
        }
        Ok(())
    }

    /// The bytes written so far and not yet taken (a few bits may be held back).
    pub fn take(&mut self) -> Vec<u8> {
        let cap = self.w.out.capacity().min(1 << 20);
        std::mem::replace(&mut self.w.out, Vec::with_capacity(cap))
    }

    /// The rest of the stream: the padding after the last block, then `trailing`.
    pub fn finish(mut self, tail_pad: u8, trailing: &[u8]) -> Result<Vec<u8>> {
        let w = &mut self.w;
        if tail_pad as u32 >> w.pad_len() != 0 {
            return Err(Error::Invalid("padding wider than its bits"));
        }
        w.put(w.pad_len(), tail_pad as u32);
        w.flush();
        w.out.extend_from_slice(trailing);
        Ok(std::mem::take(&mut w.out))
    }
}

/// Writes a block's tokens and its end; returns the bytes they cover.
///
/// The bit buffer is flushed once per token, branch-free: its 8 bytes are stored at the end
/// of the output (in capacity reserved for the whole block) and the output grows by the
/// whole bytes among them. A token takes at most 15 + 5 + 15 + 13 = 48 bits, so with
/// fewer than 8 pending the buffer never overflows.
fn write_tokens(
    w: &mut Bits,
    toks: &[Token],
    lt: &[(u32, u32)],
    dt: &[(u32, u32)],
) -> Result<usize> {
    // Up to 6 bytes per token and the end of block, and the 8 of the last store.
    w.out.reserve(toks.len() * 6 + 16);
    let base = w.out.as_mut_ptr();
    let mut len = w.out.len();
    let (mut acc, mut n) = (w.acc, w.n);
    // Whole bytes out first, so that fewer than 8 bits are pending.
    while n >= 8 {
        // SAFETY: within the capacity reserved above.
        unsafe { *base.add(len) = acc as u8 };
        len += 1;
        acc >>= 8;
        n -= 8;
    }
    let mut span = 0;
    let mut ok = true;
    for &t in toks {
        match t {
            Token::Lit(b) => {
                let (c, l) = lt[b as usize];
                ok &= l > 0;
                acc |= (c as u64) << n;
                n += l;
                span += 1;
            }
            Token::Match {
                len: ml,
                dist,
                alt258,
            } => {
                let lc = if alt258 {
                    27
                } else {
                    LEN_CODE[ml as usize] as usize
                };
                let (c, l) = lt[257 + lc];
                ok &= l > 0;
                acc |= (c as u64) << n;
                n += l;
                acc |= ((ml - LEN_BASE[lc]) as u64) << n;
                n += LEN_EXTRA[lc] as u32;
                let dc = dist_code(dist as usize);
                let (c, l) = dt[dc];
                ok &= l > 0;
                acc |= (c as u64) << n;
                n += l;
                acc |= ((dist - DIST_BASE[dc]) as u64) << n;
                n += DIST_EXTRA[dc] as u32;
                span += ml as usize;
            }
        }
        // SAFETY: `len` grows by at most 6 per token, and the capacity covers 6 per token
        // and 8 more.
        unsafe { (base.add(len) as *mut u64).write_unaligned(acc.to_le()) };
        let k = n >> 3;
        len += k as usize;
        acc >>= k * 8;
        n &= 7;
    }
    // SAFETY: the bytes up to `len` are written.
    unsafe { w.out.set_len(len) };
    (w.acc, w.n) = (acc, n);
    let (c, l) = lt[256];
    if !ok || l == 0 {
        return Err(Error::Invalid("symbol without a code"));
    }
    w.put(l, c);
    Ok(span)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_tables() {
        for l in 3..=258u16 {
            assert_eq!(LEN_CODE[l as usize] as usize, huffman::len_code(l), "{l}");
        }
        for d in 1..=32768u16 {
            assert_eq!(dist_code(d as usize), huffman::dist_code(d), "{d}");
        }
    }

    /// Padding values that do not fit in the padding bits are refused, not ORed into the
    /// bits that follow.
    #[test]
    fn wide_padding_is_refused() {
        let block = |last, body| crate::deflate::Block { last, body };
        // After a block header (3 bits), 5 bits pad a stored block.
        for (pad, ok) in [(31, true), (32, false)] {
            let s = Stream {
                blocks: vec![block(true, Body::Stored { pad, len: 0 })],
                tail_pad: 0,
                trailing: vec![],
            };
            assert_eq!(write(&s, &[0, 0], &[], &[]).is_ok(), ok, "{pad}");
        }
        // A fixed block of no tokens ends 10 bits in: 6 bits of tail padding.
        for (pad, ok) in [(63, true), (64, false)] {
            let s = Stream {
                blocks: vec![block(true, Body::Fixed { tokens: vec![] })],
                tail_pad: pad,
                trailing: vec![],
            };
            assert_eq!(write(&s, &[0, 0], &[], &[]).is_ok(), ok, "{pad}");
        }
    }
}
