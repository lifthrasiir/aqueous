//! MSZIP blocks (MS-MCI), as CAB folders hold them: each block `CK` and a DEFLATE stream of its
//! own, its last DEFLATE block final, whose matches may reach into the blocks before (the window
//! of 32 KiB goes on from block to block). The blocks of a folder are thus one DEFLATE stream but
//! for the final flags and the byte padding after each block's last: [`join`] makes that stream
//! of them (the *joined* stream, which aqr-flate takes as any other), and [`cut_onto`] the blocks
//! back from it, given how many DEFLATE blocks each holds.
//!
//! Neither decodes the data: the bodies of Huffman blocks are walked symbol by symbol and copied
//! bit for bit, and stored blocks are written again where they land, their padding (zero, as
//! everything that is taken pads with) by where they start.

use crate::{Error, Result, corrupt};

/// The signature of a block.
pub const SIG: &[u8; 2] = b"CK";

/// Bits read from a byte slice, the lowest of each byte first (DEFLATE's order).
pub struct BitR<'a> {
    d: &'a [u8],
    pos: usize,
}

impl<'a> BitR<'a> {
    pub fn new(d: &'a [u8]) -> BitR<'a> {
        BitR { d, pos: 0 }
    }

    /// The bit position.
    pub fn pos(&self) -> usize {
        self.pos
    }

    /// Up to 32 bits from bit `pos` of `d`, those past its end zero.
    #[inline]
    fn peek_at(d: &[u8], pos: usize, n: u32) -> u32 {
        let byte = pos >> 3;
        let w = match d.get(byte..byte + 8) {
            Some(b) => u64::from_le_bytes(b.try_into().unwrap()),
            None => {
                let mut b = [0u8; 8];
                let tail = d.get(byte..).unwrap_or(&[]);
                b[..tail.len()].copy_from_slice(tail);
                u64::from_le_bytes(b)
            }
        };
        ((w >> (pos & 7)) & ((1u64 << n) - 1)) as u32
    }

    #[inline]
    fn peek(&self, n: u32) -> u32 {
        BitR::peek_at(self.d, self.pos, n)
    }

    #[inline]
    fn consume(&mut self, n: u32) -> Result<()> {
        self.pos += n as usize;
        if self.pos > self.d.len() * 8 {
            return Err(Error("stream cut short"));
        }
        Ok(())
    }

    #[inline]
    fn bits(&mut self, n: u32) -> Result<u32> {
        let v = self.peek(n);
        self.consume(n)?;
        Ok(v)
    }

    /// Bits up to the next byte boundary.
    fn pad_len(&self) -> u32 {
        ((8 - (self.pos & 7)) & 7) as u32
    }

    /// Skips the padding to the next byte, which must be zero.
    fn align(&mut self) -> Result<()> {
        match self.bits(self.pad_len())? {
            0 => Ok(()),
            _ => Err(Error("padding not zero")),
        }
    }

    fn bytes(&mut self, n: usize) -> Result<&'a [u8]> {
        debug_assert!(self.pos & 7 == 0);
        let s = self.d.get(self.pos / 8..self.pos / 8 + n).ok_or(Error("stream cut short"))?;
        self.pos += 8 * n;
        Ok(s)
    }
}

/// Bits written onto a vector, the lowest of each byte first.
pub struct BitW {
    pub out: Vec<u8>,
    acc: u64,
    n: u32,
}

impl BitW {
    pub fn new(out: Vec<u8>) -> BitW {
        BitW { out, acc: 0, n: 0 }
    }

    #[inline]
    fn bits(&mut self, n: u32, v: u32) {
        self.acc |= (v as u64) << self.n;
        self.n += n;
        if self.n >= 32 {
            self.out.extend_from_slice(&(self.acc as u32).to_le_bytes());
            self.acc >>= 32;
            self.n -= 32;
        }
    }

    /// Pads with zeros to the next byte and writes out the bits pending.
    pub fn align(&mut self) {
        while self.n > 0 {
            self.out.push(self.acc as u8);
            self.acc >>= 8;
            self.n = self.n.saturating_sub(8);
        }
        self.acc = 0;
    }

    /// Bits `from..to` of `d`.
    fn copy(&mut self, d: &[u8], from: usize, to: usize) {
        let mut p = from;
        if self.n == 0 && p & 7 == 0 {
            let (a, b) = (p / 8, to / 8);
            self.out.extend_from_slice(&d[a..b]);
            p = 8 * b;
        }
        while to - p >= 32 {
            self.bits(32, BitR::peek_at(d, p, 32));
            p += 32;
        }
        let n = (to - p) as u32;
        self.bits(n, BitR::peek_at(d, p, n));
    }
}

/// Codes whose lengths are at most this are decoded by one look-up.
const FAST: u32 = 10;

/// A canonical Huffman code to decode: by a table of the next [`FAST`] bits (symbol and length,
/// none where the code is longer), else bit by bit.
struct Code {
    fast: Vec<u16>,
    count: [u16; 16],
    syms: Vec<u16>,
}

impl Code {
    /// The code of `lens` (an incomplete code is taken; what it lacks fails to decode).
    fn new(lens: &[u8]) -> Result<Code> {
        let mut count = [0u16; 16];
        lens.iter().for_each(|&l| count[l as usize] += 1);
        count[0] = 0;
        let mut left = 1i32;
        for &c in &count[1..] {
            left = 2 * left - c as i32;
            if left < 0 {
                return Err(Error("oversubscribed code"));
            }
        }
        let mut offs = [0u16; 16];
        for l in 1..15 {
            offs[l + 1] = offs[l] + count[l];
        }
        let mut syms = vec![0u16; lens.len()];
        let mut fast = vec![0u16; 1 << FAST];
        let mut next = [0u32; 16];
        let mut code = 0u32;
        for l in 1..16 {
            code = (code + count[l - 1] as u32) << 1;
            next[l] = code;
        }
        for (s, &l) in lens.iter().enumerate() {
            if l == 0 {
                continue;
            }
            let l = l as usize;
            syms[offs[l] as usize] = s as u16;
            offs[l] += 1;
            let c = next[l];
            next[l] += 1;
            if l as u32 <= FAST {
                let rev = c.reverse_bits() >> (32 - l);
                let mut i = rev as usize;
                while i < fast.len() {
                    fast[i] = (s as u16) << 4 | l as u16;
                    i += 1 << l;
                }
            }
        }
        Ok(Code { fast, count, syms })
    }

    #[inline]
    fn decode(&self, r: &mut BitR) -> Result<u16> {
        let v = r.peek(15);
        let e = self.fast[(v & ((1 << FAST) - 1)) as usize];
        if e != 0 {
            r.consume((e & 15) as u32)?;
            return Ok(e >> 4);
        }
        // (bit by bit, as the code's first codes of each length tell)
        let (mut code, mut first, mut index) = (0i32, 0i32, 0i32);
        for l in 1..16 {
            code |= ((v >> (l - 1)) & 1) as i32;
            let c = self.count[l] as i32;
            if code - first < c {
                r.consume(l as u32)?;
                return Ok(self.syms[(index + code - first) as usize]);
            }
            index += c;
            first = (first + c) << 1;
            code <<= 1;
        }
        Err(Error("undecodable code"))
    }
}

const LEN_EXTRA: [u8; 29] = [0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0];
const LEN_BASE: [u16; 29] =
    [3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131, 163, 195, 227, 258];
const DIST_EXTRA: [u8; 30] =
    [0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13, 13];
const CL_ORDER: [usize; 19] = [16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15];

/// The codes of fixed blocks.
fn fixed() -> &'static (Code, Code) {
    static F: std::sync::OnceLock<(Code, Code)> = std::sync::OnceLock::new();
    F.get_or_init(|| {
        let mut lit = [8u8; 288];
        lit[144..256].fill(9);
        lit[256..280].fill(7);
        (Code::new(&lit).unwrap(), Code::new(&[5; 30]).unwrap())
    })
}

/// The codes of a dynamic block's header.
fn header(r: &mut BitR) -> Result<(Code, Code)> {
    let hlit = r.bits(5)? as usize + 257;
    let hdist = r.bits(5)? as usize + 1;
    let hclen = r.bits(4)? as usize + 4;
    let mut cl = [0u8; 19];
    for &s in &CL_ORDER[..hclen] {
        cl[s] = r.bits(3)? as u8;
    }
    let cl = Code::new(&cl)?;
    let mut lens = [0u8; 320];
    let mut n = 0;
    while n < hlit + hdist {
        let (v, run) = match cl.decode(r)? {
            s @ 0..=15 => (s as u8, 1),
            16 => (*lens[..n].last().ok_or_else(corrupt)?, 3 + r.bits(2)? as usize),
            17 => (0, 3 + r.bits(3)? as usize),
            18 => (0, 11 + r.bits(7)? as usize),
            _ => return Err(corrupt()),
        };
        if n + run > hlit + hdist {
            return Err(corrupt());
        }
        lens[n..n + run].fill(v);
        n += run;
    }
    if lens[256] == 0 {
        return Err(Error("no end of block"));
    }
    Ok((Code::new(&lens[..hlit])?, Code::new(&lens[hlit..hlit + hdist])?))
}

/// Walks the symbols of a Huffman block's body up to its end of block: its bytes of data.
fn walk(r: &mut BitR, lit: &Code, dist: &Code) -> Result<usize> {
    let mut n = 0;
    loop {
        match lit.decode(r)? {
            s @ 0..=255 => {
                let _ = s;
                n += 1;
            }
            256 => return Ok(n),
            s @ 257..=285 => {
                let c = s as usize - 257;
                n += (LEN_BASE[c] + r.bits(LEN_EXTRA[c] as u32)? as u16) as usize;
                let d = dist.decode(r)? as usize;
                if d >= 30 {
                    return Err(corrupt());
                }
                r.consume(DIST_EXTRA[d] as u32)?;
            }
            _ => return Err(corrupt()),
        }
    }
}

/// Copies a DEFLATE block from `r` onto `w`, final there where `last`: whether it was final in
/// `r`, and its bytes of data. Stored blocks must be padded with zeros.
pub fn block(r: &mut BitR, w: &mut BitW, last: bool) -> Result<(bool, usize)> {
    let fin = r.bits(1)? == 1;
    let kind = r.bits(2)?;
    w.bits(1, last as u32);
    w.bits(2, kind);
    let n = match kind {
        0 => {
            r.align()?;
            let len = r.bits(16)?;
            if r.bits(16)? != len ^ 0xffff {
                return Err(corrupt());
            }
            w.align();
            w.bits(16, len);
            w.bits(16, len ^ 0xffff);
            w.align();
            w.out.extend_from_slice(r.bytes(len as usize)?);
            len as usize
        }
        1 | 2 => {
            let start = r.pos;
            let n = match kind {
                1 => walk(r, &fixed().0, &fixed().1)?,
                _ => {
                    let (lit, dist) = header(r)?;
                    walk(r, &lit, &dist)?
                }
            };
            w.copy(r.d, start, r.pos);
            n
        }
        _ => return Err(corrupt()),
    };
    Ok((fin, n))
}

/// A block of a folder as [`join`] takes it: its DEFLATE blocks, and its bytes of data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Joined {
    pub blocks: usize,
    pub data: usize,
}

/// The joined stream of MSZIP blocks `blocks` (each with its signature), onto `w`, and what each
/// was. Each must end where its stream does, padded with zeros.
pub fn join(blocks: &[&[u8]], w: &mut BitW) -> Result<Vec<Joined>> {
    let mut out = Vec::with_capacity(blocks.len());
    for (i, b) in blocks.iter().enumerate() {
        let b = b.strip_prefix(SIG).ok_or(Error("no signature"))?;
        let mut r = BitR::new(b);
        let mut j = Joined { blocks: 0, data: 0 };
        loop {
            let fin = r.peek(1) == 1;
            let (_, n) = block(&mut r, w, fin && i + 1 == blocks.len())?;
            j.blocks += 1;
            j.data += n;
            if fin {
                break;
            }
        }
        r.align()?;
        if r.pos != 8 * b.len() {
            return Err(Error("bytes after the stream"));
        }
        out.push(j);
    }
    w.align();
    Ok(out)
}

/// An MSZIP block cut from a joined stream `r` onto `out`: its signature, then `blocks` DEFLATE
/// blocks, the last final, padded to a byte. Where the last is final in `r` too (the stream's
/// end), `r` is taken to the next byte.
pub fn cut_onto(r: &mut BitR, blocks: usize, out: &mut Vec<u8>) -> Result<()> {
    out.extend_from_slice(SIG);
    let mut w = BitW::new(std::mem::take(out));
    let mut fin = false;
    for k in 0..blocks {
        if fin {
            return Err(Error("blocks past the stream's end"));
        }
        fin = block(r, &mut w, k + 1 == blocks)?.0;
    }
    w.align();
    *out = w.out;
    if fin {
        r.align()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Bits written for a test stream.
    fn bits(w: &mut BitW, v: &[(u32, u32)]) {
        v.iter().for_each(|&(n, x)| w.bits(n, x));
    }

    #[test]
    fn joins_and_cuts_stored_and_fixed() {
        // block 1: a fixed block (not final) of 'a', then a final stored block of "xyz";
        // block 2: a final fixed block of a match of 3 back 3 (fixed codes: lit 'a' = 0x30 + 'a'
        // in 8 bits, reversed; EOB 7 zero bits; length 3 = 257 in 7 bits, distance 2 = 3 back)
        let mut w = BitW::new(SIG.to_vec());
        let rev = |c: u32, l: u32| c.reverse_bits() >> (32 - l);
        bits(&mut w, &[(1, 0), (2, 1), (8, rev(0x30 + b'a' as u32, 8)), (7, 0)]);
        bits(&mut w, &[(1, 1), (2, 0)]);
        w.align();
        bits(&mut w, &[(16, 3), (16, 0xfffc)]);
        w.align();
        w.out.extend_from_slice(b"xyz");
        let b1 = w.out;
        let mut w = BitW::new(SIG.to_vec());
        bits(&mut w, &[(1, 1), (2, 1), (7, rev(1, 7)), (5, rev(2, 5)), (7, 0)]);
        w.align();
        let b2 = w.out;
        let mut j = BitW::new(Vec::new());
        let joined = join(&[&b1, &b2], &mut j).unwrap();
        assert_eq!(joined, [Joined { blocks: 2, data: 4 }, Joined { blocks: 1, data: 3 }]);
        // the joined stream: the stored block's padding by where it lands
        let mut r = BitR::new(&j.out);
        let mut out = Vec::new();
        cut_onto(&mut r, 2, &mut out).unwrap();
        assert_eq!(out, b1);
        out.clear();
        cut_onto(&mut r, 1, &mut out).unwrap();
        assert_eq!(out, b2);
        assert_eq!(r.pos(), 8 * j.out.len());
    }
}
