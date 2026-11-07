//! The streams of methods `-lh4-` to `-lh7-` (`FORMAT.md`, "Streams"): LZ77 under static Huffman
//! codes sent block by block. `decode` reads a stream into its data, its blocks' headers as they
//! are written and its tokens; `encode` writes a stream of them again, so that a stream whose
//! parse encodes to itself is its parse.

use crate::{Error, Result, corrupt};
use aqr_common::msb::ZeroReader;

/// Symbols of the code of literals and lengths: 256 literals, lengths 3 to 256.
pub const NC: usize = 510;
/// Symbols of the code of the code lengths of `NC`.
pub const NT: usize = 19;
/// Bits of the count of `NC`'s lengths, and of a long run of zeros.
const CBIT: u32 = 9;
/// Bits of the count of `NT`'s lengths.
const TBIT: u32 = 5;
/// The shortest match.
pub const MIN_MATCH: usize = 3;
/// The longest match.
pub const MAX_MATCH: usize = 256;
/// The longest code.
pub const MAX_LEN: u8 = 16;

/// A method of the family: its window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Method {
    /// Bits of the window: 12 (`-lh4-`), 13 (`-lh5-`), 15 (`-lh6-`), 16 (`-lh7-`).
    pub dicbit: u8,
}

impl Method {
    /// The method of a header's method id (`-lh4-` and so on).
    pub fn of(id: &[u8]) -> Option<Method> {
        let dicbit = match id {
            b"-lh4-" => 12,
            b"-lh5-" => 13,
            b"-lh6-" => 15,
            b"-lh7-" => 16,
            _ => return None,
        };
        Some(Method { dicbit })
    }

    /// The method of its number in records: 4 to 7.
    pub fn of_number(n: u8) -> Option<Method> {
        Method::of(&[b'-', b'l', b'h', n.wrapping_add(b'0'), b'-'])
    }

    pub fn number(self) -> u8 {
        match self.dicbit {
            12 => 4,
            13 => 5,
            15 => 6,
            _ => 7,
        }
    }

    /// Symbols of the code of distances.
    pub fn np(self) -> usize {
        match self.dicbit {
            12 | 13 => 14,
            d => d as usize + 1,
        }
    }

    /// Bits of the count of the distances' lengths.
    pub fn pbit(self) -> u32 {
        if self.dicbit <= 13 { 4 } else { 5 }
    }

    /// The farthest a match reaches back.
    pub fn window(self) -> usize {
        1 << self.dicbit
    }
}

/// A table of code lengths as `read_pt_len` reads it (the codes of `NT` and of distances).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PtTable {
    /// The count written; 0 for a code of one symbol, `single`, of no bits.
    pub n: u16,
    pub single: u16,
    /// The lengths read (zeros after them up to the alphabet's size).
    pub lens: Vec<u8>,
    /// For `NT`: the zeros the 2 bits after the third length tell (0-3), at `lens[3..]`.
    pub skip: u8,
}

/// An item of the table of `NC`'s lengths: a symbol of `NT` and its extra bits (a run of zeros:
/// symbol 0 is one, 1 is 3-18 by 4 bits, 2 is 20-531 by 9; symbols 3-18 are lengths 1-16).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Item {
    pub sym: u8,
    pub extra: u16,
}

impl Item {
    /// The lengths it stands for: zeros, or one length.
    pub fn zeros(self) -> usize {
        match self.sym {
            0 => 1,
            1 => self.extra as usize + 3,
            2 => self.extra as usize + 20,
            _ => 0,
        }
    }
}

/// The table of `NC`'s lengths as `read_c_len` reads it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CTable {
    /// The count written; 0 for a code of one symbol, `single`.
    pub n: u16,
    pub single: u16,
    pub items: Vec<Item>,
}

/// A block's header: its count of tokens and its three tables, as written.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Head {
    pub size: u16,
    pub t: PtTable,
    pub c: CTable,
    pub p: PtTable,
}

/// A token: a literal, or a match of `len` (3-256) bytes `dist` (1 to the window) back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Token {
    Literal(u8),
    Match { len: u16, dist: u32 },
}

/// A stream read.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Stream {
    pub data: Vec<u8>,
    pub heads: Vec<Head>,
    pub tokens: Vec<Token>,
    /// The bits after the last token up to the byte's end (`pad_bits` of them), and the bytes
    /// after that.
    pub pad: u8,
    pub pad_bits: u8,
    pub tail: Vec<u8>,
}

/// The lengths of `NC`'s code that `c` tells (none for a code of one symbol).
pub fn c_lens(c: &CTable) -> Result<Vec<u8>> {
    let mut lens = Vec::with_capacity(NC);
    for it in &c.items {
        match it.sym {
            0..=2 => lens.resize(lens.len() + it.zeros(), 0),
            s => lens.push(s - 2),
        }
    }
    if lens.len() > NC {
        return Err(Error("a table past its symbols"));
    }
    lens.resize(NC, 0);
    Ok(lens)
}

/// The lengths of a code of `nn` symbols that `t` tells.
pub fn pt_lens(t: &PtTable, nn: usize) -> Result<Vec<u8>> {
    let mut lens = t.lens.clone();
    if lens.len() > nn {
        return Err(Error("a table past its symbols"));
    }
    lens.resize(nn, 0);
    Ok(lens)
}

/// A canonical code: each symbol's code and length, shorter codes first, by symbol within a
/// length; with the lookup of the decoder. Over-full codes are refused; incomplete ones are
/// taken (a code missing fails when read).
pub struct Code {
    pub codes: Vec<u16>,
    pub lens: Vec<u8>,
    /// For a code of one symbol written so: that symbol, of no bits.
    pub single: Option<u16>,
    /// Lookup of 16 bits: (symbol, length), length 0 where no code.
    first: [u32; 17],
    count: [u16; 17],
    sorted: Vec<u16>,
}

impl Code {
    pub fn new(lens: &[u8]) -> Result<Code> {
        let mut count = [0u16; 17];
        for &l in lens {
            if l > MAX_LEN {
                return Err(Error("a code too long"));
            }
            count[l as usize] += 1;
        }
        count[0] = 0;
        let mut first = [0u32; 17];
        let mut code = 0u32;
        for l in 1..=16 {
            first[l] = code;
            code = (code + count[l] as u32) << 1;
            if first[l] + count[l] as u32 > 1 << l {
                return Err(Error("an over-full code"));
            }
        }
        let mut next = first;
        let mut codes = vec![0u16; lens.len()];
        let mut sorted = Vec::new();
        for (l, nx) in next.iter_mut().enumerate().skip(1) {
            for (s, &x) in lens.iter().enumerate() {
                if x as usize == l {
                    codes[s] = *nx as u16;
                    *nx += 1;
                    sorted.push(s as u16);
                }
            }
        }
        Ok(Code { codes, lens: lens.to_vec(), single: None, first, count, sorted })
    }

    fn single(s: u16, n: usize) -> Code {
        Code {
            codes: vec![0; n],
            lens: vec![0; n],
            single: Some(s),
            first: [0; 17],
            count: [0; 17],
            sorted: Vec::new(),
        }
    }

    fn read(&self, r: &mut Reader) -> Result<u16> {
        if let Some(s) = self.single {
            return Ok(s);
        }
        let mut code = 0u32;
        let mut idx = 0usize;
        for l in 1..=16 {
            code = code << 1 | r.bit()?;
            let c = self.count[l] as u32;
            if code < self.first[l] + c {
                return Ok(self.sorted[idx + (code - self.first[l]) as usize]);
            }
            idx += c as usize;
        }
        Err(Error("no such code"))
    }

    fn write(&self, w: &mut Writer, s: usize) -> Result<()> {
        if self.single.is_some() {
            return if self.single == Some(s as u16) { Ok(()) } else { Err(Error("a symbol not in the code")) };
        }
        let l = *self.lens.get(s).ok_or_else(corrupt)?;
        if l == 0 {
            return Err(Error("a symbol not in the code"));
        }
        w.put(l as u32, self.codes[s] as u32);
        Ok(())
    }
}

/// MSB-first reader; past the end it fails.
pub struct Reader<'a>(ZeroReader<'a>);

impl<'a> Reader<'a> {
    pub fn new(d: &'a [u8]) -> Self {
        Reader(ZeroReader::new(d, 0))
    }

    /// The position in bits.
    pub fn pos(&self) -> usize {
        self.0.pos
    }

    #[inline]
    fn bit(&mut self) -> Result<u32> {
        self.get(1)
    }

    #[inline]
    pub fn get(&mut self, n: u32) -> Result<u32> {
        let v = self.0.get(n);
        if self.0.overrun() {
            return Err(Error("a stream cut short"));
        }
        Ok(v)
    }
}

/// MSB-first writer.
#[derive(Default)]
pub struct Writer {
    pub out: Vec<u8>,
    acc: u64,
    n: u32,
}

impl Writer {
    /// A writer appending to the end of `out`.
    pub fn onto(out: Vec<u8>) -> Writer {
        Writer { out, acc: 0, n: 0 }
    }

    #[inline]
    pub fn put(&mut self, n: u32, v: u32) {
        debug_assert!(n <= 32);
        if n == 0 {
            return;
        }
        self.acc = self.acc << n | (v as u64 & ((1u64 << n) - 1));
        self.n += n;
        while self.n >= 8 {
            self.n -= 8;
            self.out.push((self.acc >> self.n) as u8);
        }
    }

    /// The bits written past the last byte.
    pub fn pending(&self) -> u32 {
        self.n
    }
}

fn read_pt(r: &mut Reader, nn: usize, nbit: u32, special: Option<usize>) -> Result<(PtTable, Code)> {
    let n = r.get(nbit)? as usize;
    if n == 0 {
        let s = r.get(nbit)? as u16;
        let t = PtTable { n: 0, single: s, lens: Vec::new(), skip: 0 };
        return Ok((t, Code::single(s, nn)));
    }
    if n > nn {
        return Err(Error("a table past its symbols"));
    }
    let mut lens = Vec::with_capacity(nn);
    let mut skip = 0;
    while lens.len() < n {
        let mut c = r.get(3)? as u8;
        if c == 7 {
            while r.bit()? == 1 {
                c += 1;
                if c > MAX_LEN {
                    return Err(Error("a code too long"));
                }
            }
        }
        lens.push(c);
        if Some(lens.len()) == special {
            skip = r.get(2)? as u8;
            lens.resize(lens.len() + skip as usize, 0);
        }
    }
    if lens.len() > nn {
        return Err(Error("a table past its symbols"));
    }
    let t = PtTable { n: n as u16, single: 0, lens, skip };
    let code = Code::new(&pt_lens(&t, nn)?)?;
    Ok((t, code))
}

fn write_pt(w: &mut Writer, t: &PtTable, nbit: u32, special: Option<usize>) {
    w.put(nbit, t.n as u32);
    if t.n == 0 {
        w.put(nbit, t.single as u32);
        return;
    }
    let mut i = 0;
    while i < t.n as usize {
        let k = t.lens[i];
        i += 1;
        if k <= 6 {
            w.put(3, k as u32);
        } else {
            w.put(k as u32 - 3, (1 << (k - 3)) - 2);
        }
        if Some(i) == special {
            w.put(2, t.skip as u32);
            i += t.skip as usize;
        }
    }
}

fn read_c(r: &mut Reader, t: &Code) -> Result<(CTable, Code)> {
    let n = r.get(CBIT)? as usize;
    if n == 0 {
        let s = r.get(CBIT)? as u16;
        return Ok((CTable { n: 0, single: s, items: Vec::new() }, Code::single(s, NC)));
    }
    if n > NC {
        return Err(Error("a table past its symbols"));
    }
    let mut items = Vec::new();
    let mut i = 0;
    while i < n {
        let sym = t.read(r)? as u8;
        let extra = match sym {
            1 => r.get(4)? as u16,
            2 => r.get(CBIT)? as u16,
            _ => 0,
        };
        let it = Item { sym, extra };
        i += if sym <= 2 { it.zeros() } else { 1 };
        items.push(it);
    }
    let c = CTable { n: n as u16, single: 0, items };
    let code = Code::new(&c_lens(&c)?)?;
    Ok((c, code))
}

fn write_c(w: &mut Writer, c: &CTable, t: &Code) -> Result<()> {
    w.put(CBIT, c.n as u32);
    if c.n == 0 {
        w.put(CBIT, c.single as u32);
        return Ok(());
    }
    for it in &c.items {
        t.write(w, it.sym as usize)?;
        match it.sym {
            1 => w.put(4, it.extra as u32),
            2 => w.put(CBIT, it.extra as u32),
            _ => {}
        }
    }
    Ok(())
}

/// The codes of a block's header.
pub struct Codes {
    pub t: Code,
    pub c: Code,
    pub p: Code,
}

/// Reads a block's header.
pub fn read_head(r: &mut Reader, m: Method) -> Result<(Head, Codes)> {
    let size = r.get(16)? as u16;
    if size == 0 {
        return Err(Error("an empty block"));
    }
    let (t, tc) = read_pt(r, NT, TBIT, Some(3))?;
    let (c, cc) = read_c(r, &tc)?;
    let (p, pc) = read_pt(r, m.np(), m.pbit(), None)?;
    Ok((Head { size, t, c, p }, Codes { t: tc, c: cc, p: pc }))
}

/// The codes of `h`, checking that its tables are codes and its counts within their alphabets.
pub fn codes(h: &Head, m: Method) -> Result<Codes> {
    let code = |t: &PtTable, nn: usize, nbit: u32| -> Result<Code> {
        if t.n == 0 {
            if t.single as u32 >= 1 << nbit || !t.lens.is_empty() || t.skip != 0 {
                return Err(corrupt());
            }
            return Ok(Code::single(t.single, nn));
        }
        if t.n as u32 >= 1 << nbit || t.n as usize > nn || t.single != 0 {
            return Err(corrupt());
        }
        Code::new(&pt_lens(t, nn)?)
    };
    // (the lengths as they would be read again: as many as the count, the skipped zeros)
    let pt_ok = |t: &PtTable, special: Option<usize>| -> bool {
        if t.n == 0 {
            return true;
        }
        let mut i = 0;
        while i < t.n as usize {
            if t.lens.get(i).is_none_or(|&l| l > MAX_LEN) {
                return false;
            }
            i += 1;
            if Some(i) == special {
                if t.skip > 3 || t.lens.get(i..i + t.skip as usize).is_none_or(|z| z.iter().any(|&l| l != 0)) {
                    return false;
                }
                i += t.skip as usize;
            }
        }
        i == t.lens.len() && (special.is_some() || t.skip == 0)
    };
    if !pt_ok(&h.t, Some(3)) || !pt_ok(&h.p, None) || h.size == 0 {
        return Err(corrupt());
    }
    let t = code(&h.t, NT, TBIT)?;
    let c = if h.c.n == 0 {
        if h.c.single as usize >= NC || !h.c.items.is_empty() {
            return Err(corrupt());
        }
        Code::single(h.c.single, NC)
    } else {
        // (the items fill the count exactly but for the last run of zeros)
        let mut i = 0;
        for (k, it) in h.c.items.iter().enumerate() {
            if i >= h.c.n as usize || it.sym as usize >= NT || (it.sym > 2 && it.extra != 0) {
                return Err(corrupt());
            }
            if (it.sym == 1 && it.extra > 15) || (it.sym == 2 && it.extra >= 1 << CBIT) {
                return Err(corrupt());
            }
            i += if it.sym <= 2 { it.zeros() } else { 1 };
            if i >= h.c.n as usize && k + 1 != h.c.items.len() {
                return Err(corrupt());
            }
        }
        if i < h.c.n as usize || h.c.n as usize > NC {
            return Err(corrupt());
        }
        Code::new(&c_lens(&h.c)?)?
    };
    let p = code(&h.p, m.np(), m.pbit())?;
    Ok(Codes { t, c, p })
}

pub fn write_head(w: &mut Writer, h: &Head, m: Method, k: &Codes) -> Result<()> {
    w.put(16, h.size as u32);
    write_pt(w, &h.t, TBIT, Some(3));
    write_c(w, &h.c, &k.t)?;
    write_pt(w, &h.p, m.pbit(), None);
    Ok(())
}

/// The distance's symbol and extra bits: `j = bitlen(dist − 1)`, then the bits of `dist − 1`
/// below its top one.
#[inline]
fn dist_code(dist: u32) -> (usize, u32, u32) {
    let v = dist - 1;
    let j = (u32::BITS - v.leading_zeros()) as usize;
    if j <= 1 { (j, 0, 0) } else { (j, j as u32 - 1, v - (1 << (j - 1))) }
}

/// Reads the stream `d` of `size` bytes of data.
pub fn decode(d: &[u8], size: usize, m: Method) -> Result<Stream> {
    let mut r = Reader::new(d);
    let mut st = Stream { data: Vec::with_capacity(size), ..Stream::default() };
    let window = m.window();
    while st.data.len() < size {
        let (h, k) = read_head(&mut r, m)?;
        for _ in 0..h.size {
            if st.data.len() >= size {
                return Err(Error("a block past the data"));
            }
            let c = k.c.read(&mut r)? as usize;
            if c < 256 {
                st.data.push(c as u8);
                st.tokens.push(Token::Literal(c as u8));
                continue;
            }
            let len = c - 256 + MIN_MATCH;
            let j = k.p.read(&mut r)? as u32;
            let v = if j <= 1 { j } else { (1 << (j - 1)) + r.get(j - 1)? };
            let dist = v as usize + 1;
            let pos = st.data.len();
            if dist > pos || dist > window || pos + len > size {
                return Err(Error("a match out of the data"));
            }
            for i in 0..len {
                let b = st.data[pos - dist + i];
                st.data.push(b);
            }
            st.tokens.push(Token::Match { len: len as u16, dist: dist as u32 });
        }
        st.heads.push(h);
    }
    let bits = (8 - r.pos() % 8) % 8;
    st.pad_bits = bits as u8;
    st.pad = r.get(bits as u32)? as u8;
    st.tail = d[r.pos() / 8..].to_vec();
    Ok(st)
}

/// The tokens' symbols counted: of `NC`, and of distances (`np` of them).
pub fn freqs(tokens: &[Token], np: usize) -> (Vec<u32>, Vec<u32>) {
    let (mut c, mut p) = (vec![0u32; NC], vec![0u32; np]);
    for t in tokens {
        match *t {
            Token::Literal(b) => c[b as usize] += 1,
            Token::Match { len, dist } => {
                c[256 + len as usize - MIN_MATCH] += 1;
                let j = dist_code(dist).0;
                if j < np {
                    p[j] += 1;
                }
            }
        }
    }
    (c, p)
}

/// Writes a block of `h` and its tokens onto `w`.
pub fn write_block(w: &mut Writer, h: &Head, k: &Codes, tokens: &[Token], m: Method) -> Result<()> {
    if tokens.len() != h.size as usize {
        return Err(corrupt());
    }
    write_head(w, h, m, k)?;
    for t in tokens {
        match *t {
            Token::Literal(b) => k.c.write(w, b as usize)?,
            Token::Match { len, dist } => {
                let len = len as usize;
                if !(MIN_MATCH..=MAX_MATCH).contains(&len) || dist == 0 || dist as usize > m.window() {
                    return Err(corrupt());
                }
                k.c.write(w, 256 + len - MIN_MATCH)?;
                let (j, nb, extra) = dist_code(dist);
                if j >= m.np() {
                    return Err(corrupt());
                }
                k.p.write(w, j)?;
                w.put(nb, extra);
            }
        }
    }
    Ok(())
}

/// Ends a stream written onto `w`: its padding bits and the bytes after them.
pub fn finish(mut w: Writer, pad: u8, pad_bits: u8, tail: &[u8]) -> Result<Vec<u8>> {
    pad_onto(&mut w, pad, pad_bits)?;
    w.out.extend_from_slice(tail);
    Ok(w.out)
}

/// The padding bits of a stream written onto `w`: `pad_bits` of them, as many as it needs.
pub fn pad_onto(w: &mut Writer, pad: u8, pad_bits: u8) -> Result<()> {
    let want = (8 - w.pending() % 8) % 8;
    if pad_bits as u32 != want || (pad as u32) >> pad_bits != 0 {
        return Err(corrupt());
    }
    w.put(want, pad as u32);
    Ok(())
}

/// The stream of `st`'s headers and tokens (its data unused).
pub fn encode(st: &Stream, m: Method) -> Result<Vec<u8>> {
    let mut w = Writer::default();
    let mut at = 0;
    for h in &st.heads {
        let k = codes(h, m)?;
        let n = h.size as usize;
        let toks = st.tokens.get(at..at + n).ok_or_else(corrupt)?;
        write_block(&mut w, h, &k, toks, m)?;
        at += n;
    }
    if at != st.tokens.len() {
        return Err(corrupt());
    }
    finish(w, st.pad, st.pad_bits, &st.tail)
}
