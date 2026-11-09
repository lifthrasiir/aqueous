//! XPRESS Huffman ([MS-XCA] 2.1, 2.2), read into its parse and written back from it. `FORMAT.md`
//! §2 describes the stream.
//!
//! A stream of bytes is blocks, each a table of 512 code lengths (256 bytes, 4 bits each, the
//! even symbol in the low nibble) and then the bits of its tokens, in words of 16 bits with the
//! bytes of long matches' lengths between them. A block ends after the token that brings the
//! data it holds to [`BLOCK`] bytes or more; the last also holds the end symbol.

/// Bytes of data after which a block ends, at the end of the token that reaches them.
pub const BLOCK: usize = 65536;
/// The shortest match.
pub const MIN_MATCH: usize = 3;
/// The farthest back a match reaches.
pub const MAX_OFF: usize = 65535;
/// Symbols of the code: 256 literals, then 16 lengths for each of 32 offsets' bit lengths.
pub const SYMS: usize = 512;
/// The longest code.
pub const MAX_BITS: u8 = 15;
/// The symbol that ends the stream (and, otherwise, a match of 3 bytes at offset 1).
pub const END: usize = 256;

/// A token: a literal (`len` 0) or a match of `len` bytes (at least [`MIN_MATCH`]) back `off`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Tok {
    pub len: u32,
    pub off: u32,
}

impl Tok {
    pub const LIT: Tok = Tok { len: 0, off: 0 };

    /// Bytes of data the token holds.
    #[inline]
    pub fn size(self) -> usize {
        if self.len == 0 { 1 } else { self.len as usize }
    }
}

/// A block's code lengths and the tokens it holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockInfo {
    pub lens: Box<[u8; SYMS]>,
    pub toks: usize,
}

/// A chunk read: its blocks and their tokens, and whether the end symbol follows the last.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Parse {
    pub blocks: Vec<BlockInfo>,
    pub toks: Vec<Tok>,
    pub end: bool,
}

/// Whether `lens` is a complete prefix code (all the symbols' lengths at most [`MAX_BITS`]).
pub fn complete(lens: &[u8; SYMS]) -> bool {
    let mut sum = 0u32;
    for &l in lens {
        if l > MAX_BITS {
            return false;
        }
        if l != 0 {
            sum += 1 << (MAX_BITS - l);
        }
    }
    sum == 1 << MAX_BITS
}

/// The canonical codes of `lens` (by length, then symbol): None unless complete.
pub fn codes(lens: &[u8; SYMS]) -> Option<[u16; SYMS]> {
    if !complete(lens) {
        return None;
    }
    let mut count = [0u32; 17];
    for &l in lens {
        count[l as usize] += 1;
    }
    count[0] = 0;
    let mut next = [0u32; 17];
    for l in 1..=MAX_BITS as usize {
        next[l] = (next[l - 1] + count[l - 1]) << 1;
    }
    let mut out = [0u16; SYMS];
    for (s, &l) in lens.iter().enumerate() {
        if l != 0 {
            out[s] = next[l as usize] as u16;
            next[l as usize] += 1;
        }
    }
    Some(out)
}

/// Bits of the fast table of the decoder.
const FAST: u32 = 11;

/// The decoder of a code: a table for the codes of up to [`FAST`] bits, the canonical ranges for
/// the rest.
struct Dec {
    fast: [u16; 1 << FAST],
    first: [u32; 17],
    count: [u32; 17],
    offs: [u32; 17],
    order: [u16; SYMS],
}

impl Dec {
    fn new(lens: &[u8; SYMS]) -> Option<Box<Dec>> {
        if !complete(lens) {
            return None;
        }
        let mut d =
            Box::new(Dec { fast: [0; 1 << FAST], first: [0; 17], count: [0; 17], offs: [0; 17], order: [0; SYMS] });
        for &l in lens {
            d.count[l as usize] += 1;
        }
        d.count[0] = 0;
        for l in 1..=MAX_BITS as usize {
            d.first[l] = (d.first[l - 1] + d.count[l - 1]) << 1;
            d.offs[l] = d.offs[l - 1] + d.count[l - 1];
        }
        let mut at = d.offs;
        for (s, &l) in lens.iter().enumerate() {
            if l != 0 {
                d.order[at[l as usize] as usize] = s as u16;
                at[l as usize] += 1;
            }
        }
        for l in 1..=FAST as usize {
            for k in 0..d.count[l] {
                let sym = d.order[(d.offs[l] + k) as usize];
                let code = (d.first[l] + k) << (FAST as usize - l);
                for e in code..code + (1 << (FAST as usize - l)) {
                    d.fast[e as usize] = sym << 4 | l as u16;
                }
            }
        }
        Some(d)
    }

    /// The symbol and length of the code that starts `peek` (15 bits).
    #[inline]
    fn get(&self, peek: u32) -> Option<(usize, u32)> {
        let e = self.fast[(peek >> (MAX_BITS as u32 - FAST)) as usize];
        if e != 0 {
            return Some(((e >> 4) as usize, (e & 15) as u32));
        }
        for l in FAST as usize + 1..=MAX_BITS as usize {
            let code = peek >> (MAX_BITS as usize - l);
            let k = code.wrapping_sub(self.first[l]);
            if k < self.count[l] {
                return Some((self.order[(self.offs[l] + k) as usize] as usize, l as u32));
            }
        }
        None
    }
}

/// The bits of a block as the decoder reads them: 16 bits at least in the register, the words
/// and the bytes of lengths from `p` on.
struct Bits<'a> {
    src: &'a [u8],
    p: usize,
    nb: u32,
    extra: i32,
}

impl<'a> Bits<'a> {
    fn new(src: &'a [u8], p: usize) -> Option<Bits<'a>> {
        let w = src.get(p..p + 4)?;
        let nb = u16::from_le_bytes([w[0], w[1]]) as u32;
        let nb = nb << 16 | u16::from_le_bytes([w[2], w[3]]) as u32;
        Some(Bits { src, p: p + 4, nb, extra: 16 })
    }

    #[inline]
    fn skip(&mut self, n: u32) -> Option<()> {
        self.nb <<= n;
        self.extra -= n as i32;
        if self.extra < 0 {
            let w = self.src.get(self.p..self.p + 2)?;
            self.nb |= (u16::from_le_bytes([w[0], w[1]]) as u32) << -self.extra;
            self.extra += 16;
            self.p += 2;
        }
        Some(())
    }

    #[inline]
    fn byte(&mut self) -> Option<u8> {
        let b = *self.src.get(self.p)?;
        self.p += 1;
        Some(b)
    }

    fn word(&mut self) -> Option<u32> {
        let w = self.src.get(self.p..self.p + 2)?;
        self.p += 2;
        Some(u16::from_le_bytes([w[0], w[1]]) as u32)
    }

    fn dword(&mut self) -> Option<u32> {
        let w = self.src.get(self.p..self.p + 4)?;
        self.p += 4;
        Some(u32::from_le_bytes(w.try_into().unwrap()))
    }
}

/// The lengths of the table at `src[p..]`.
fn table(src: &[u8], p: usize) -> Option<Box<[u8; SYMS]>> {
    let t = src.get(p..p + 256)?;
    let mut lens = Box::new([0u8; SYMS]);
    for (i, &b) in t.iter().enumerate() {
        lens[2 * i] = b & 15;
        lens[2 * i + 1] = b >> 4;
    }
    Some(lens)
}

/// Reads a chunk `src` of `n` bytes of data: the data and the parse. None where it is not a
/// stream of this format that [`write`] gives back as it is (a code not complete, a match past
/// the data or the window, bytes after the stream, lengths not as the writer gives them).
pub fn read(src: &[u8], n: usize) -> Option<(Vec<u8>, Parse)> {
    if n == 0 {
        return None;
    }
    let mut out: Vec<u8> = Vec::with_capacity(n);
    let mut parse = Parse::default();
    let mut p = 0;
    loop {
        let lens = table(src, p)?;
        let dec = Dec::new(&lens)?;
        let mut r = Bits::new(src, p + 256)?;
        let end = out.len() + BLOCK;
        let toks_before = parse.toks.len();
        while out.len() < end && out.len() < n {
            let (sym, l) = dec.get(r.nb >> (32 - MAX_BITS as u32))?;
            r.skip(l)?;
            if sym < 256 {
                out.push(sym as u8);
                parse.toks.push(Tok::LIT);
                continue;
            }
            let s = sym - 256;
            let mut len = (s & 15) as u32;
            let ob = (s >> 4) as u32;
            if ob > 15 {
                return None;
            }
            if len == 15 {
                len = r.byte()? as u32;
                if len == 255 {
                    len = r.word()?;
                    if len == 0 {
                        len = r.dword()?;
                    }
                    len = len.checked_sub(15)?;
                }
                len += 15;
            }
            let len = len as usize + MIN_MATCH;
            let bits = if ob == 0 { 0 } else { (r.nb >> (32 - ob)) as usize };
            let off = bits + (1 << ob);
            r.skip(ob)?;
            if off > out.len() || len > n - out.len() {
                return None;
            }
            for _ in 0..len {
                out.push(out[out.len() - off]);
            }
            parse.toks.push(Tok { len: len as u32, off: off as u32 });
        }
        parse.blocks.push(BlockInfo { lens, toks: parse.toks.len() - toks_before });
        if out.len() < n {
            p = r.p;
            continue;
        }
        // the last block: the end symbol, or nothing
        let lens = &parse.blocks.last().unwrap().lens;
        let mut r2 = Bits { src: r.src, p: r.p, nb: r.nb, extra: r.extra };
        parse.end = false;
        if lens[END] != 0
            && let Some((sym, l)) = dec.get(r2.nb >> (32 - MAX_BITS as u32))
            && sym == END
            && r2.skip(l).is_some()
            && r2.p == src.len()
        {
            parse.end = true;
            r = r2;
        }
        if r.p != src.len() {
            return None;
        }
        break;
    }
    // (what the writer gives: the pad bits zero, the lengths and the words as it lays them out)
    let mut check = Vec::with_capacity(src.len());
    write(&parse, &out, &mut check)?;
    (check == src).then_some((out, parse))
}

/// The writer of a block: the words and bytes laid out as [MS-XCA] 2.1.4.3 does.
struct W<'a> {
    out: &'a mut Vec<u8>,
    p1: usize,
    p2: usize,
    free: i32,
    word: u32,
}

impl W<'_> {
    #[inline]
    fn bits(&mut self, n: u32, v: u32) {
        if self.free >= n as i32 {
            self.free -= n as i32;
            self.word = (self.word << n).wrapping_add(v);
        } else {
            self.word <<= self.free;
            self.word = self.word.wrapping_add(v >> (n as i32 - self.free));
            self.free -= n as i32;
            let w = (self.word as u16).to_le_bytes();
            self.out[self.p1..self.p1 + 2].copy_from_slice(&w);
            self.p1 = self.p2;
            self.p2 = self.out.len();
            self.out.extend_from_slice(&[0, 0]);
            self.free += 16;
            self.word = v;
        }
    }

    fn flush(&mut self) {
        self.word <<= self.free;
        let w = (self.word as u16).to_le_bytes();
        self.out[self.p1..self.p1 + 2].copy_from_slice(&w);
    }
}

/// Writes the block of `toks` over `d[at..]` onto `out` with code lengths `lens` (the end symbol
/// after the tokens where `end`); the data it holds is returned. None where a symbol has no code,
/// or `out` would pass `limit` bytes (checked at each token).
pub fn write_block(
    out: &mut Vec<u8>,
    lens: &[u8; SYMS],
    toks: &[Tok],
    d: &[u8],
    at: usize,
    end: bool,
    limit: usize,
) -> Option<usize> {
    let codes = codes(lens)?;
    let base = out.len();
    out.reserve(260);
    for i in 0..256 {
        out.push(lens[2 * i] | lens[2 * i + 1] << 4);
    }
    out.extend_from_slice(&[0; 4]);
    let mut w = W { p1: base + 256, p2: base + 258, free: 16, word: 0, out };
    let mut pos = at;
    for t in toks {
        if w.out.len() > limit {
            return None;
        }
        if t.len == 0 {
            let s = *d.get(pos)? as usize;
            if lens[s] == 0 {
                return None;
            }
            w.bits(lens[s] as u32, codes[s] as u32);
            pos += 1;
            continue;
        }
        let (len, off) = (t.len as usize, t.off as usize);
        if len < MIN_MATCH || off == 0 || off > MAX_OFF {
            return None;
        }
        let ob = off.ilog2();
        let s = 256 + (len - MIN_MATCH).min(15) + 16 * ob as usize;
        if lens[s] == 0 {
            return None;
        }
        w.bits(lens[s] as u32, codes[s] as u32);
        let l = len - MIN_MATCH;
        if l >= 15 {
            let l = l - 15;
            if l < 255 {
                w.out.push(l as u8);
            } else {
                w.out.push(255);
                let l = l + 15;
                if l < 65536 {
                    w.out.extend_from_slice(&(l as u16).to_le_bytes());
                } else {
                    w.out.extend_from_slice(&[0, 0]);
                    w.out.extend_from_slice(&u32::try_from(l).ok()?.to_le_bytes());
                }
            }
        }
        w.bits(ob, (off - (1 << ob)) as u32);
        pos += len;
    }
    if end {
        if lens[END] == 0 {
            return None;
        }
        w.bits(lens[END] as u32, codes[END] as u32);
    }
    w.flush();
    Some(pos - at)
}

/// Writes a chunk of `d` (its data) from its `parse` onto `out`: None where the parse does not
/// fit the data.
pub fn write(parse: &Parse, d: &[u8], out: &mut Vec<u8>) -> Option<()> {
    let (mut at, mut t) = (0, 0);
    for (i, b) in parse.blocks.iter().enumerate() {
        let toks = parse.toks.get(t..t + b.toks)?;
        let last = i + 1 == parse.blocks.len();
        at += write_block(out, &b.lens, toks, d, at, last && parse.end, usize::MAX)?;
        t += b.toks;
    }
    (at == d.len() && t == parse.toks.len()).then_some(())
}
