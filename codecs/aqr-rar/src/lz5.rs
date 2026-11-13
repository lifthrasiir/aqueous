//! RAR5 compressed streams (`docs/rar5-decoding.md` §2, `FORMAT.md`): decoded into their
//! unfiltered data and their tokens, blocks and code lengths as written, and written again from
//! those. The stream of a solid archive runs on through its files, each file's part a sequence
//! of blocks ending with a block marked last; the history, the codes, the distances and the
//! last length carry over from one file to the next.

use aqr_common::msb::{BitReader, BitWriter, padded};

use crate::huff::{self, Decoder};
use crate::{Error, Result, corrupt};

/// Symbols of the main code: literals, the filter, the last copy again, repeated distances,
/// lengths of explicit copies.
pub const MAIN: usize = 306;
/// Symbols of the low bits of distances, and of lengths of repeated distances.
pub const LOW: usize = 16;
pub const REPLEN: usize = 44;
/// Symbols of the length code of the code lengths.
pub const PRE: usize = 20;

/// Symbols of the distance code: 64, 80 for the algorithm of RAR 7.
pub fn dists(v1: bool) -> usize {
    if v1 { 80 } else { 64 }
}

/// Code lengths sent with tables: of the main, distance, low and repeated-length codes in turn.
pub fn lens_len(v1: bool) -> usize {
    MAIN + dists(v1) + LOW + REPLEN
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Token {
    Lit(u8),
    /// A copy of `len` bytes (with the bonus of its distance) from `dist` bytes back.
    Copy {
        len: u32,
        dist: usize,
    },
    /// A copy of `len` bytes from the distance at `idx` of the four last ones.
    Rep {
        idx: u8,
        len: u32,
    },
    /// The last copy again (its length, the last distance).
    Last,
    Filter(Filter),
}

/// A filter as declared: `start` bytes after the position declaring it, `len` bytes; its fields
/// written in `start_bytes` and `len_bytes` bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Filter {
    pub start: u32,
    pub len: u32,
    /// 0 delta, 1 x86 E8, 2 x86 E8 and E9, 3 ARM.
    pub kind: u8,
    /// The delta filter's channels, 1 to 32; 0 for the others.
    pub channels: u8,
    pub start_bytes: u8,
    pub len_bytes: u8,
}

/// The tables of a block: the length code's lengths and the code lengths it codes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tables {
    pub pre: [u8; PRE],
    pub lens: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Block {
    /// The tables sent at its start, if any.
    pub tables: Option<Tables>,
    /// Its tokens, in [`Stream::tokens`] after the blocks before.
    pub tokens: usize,
    pub last: bool,
}

/// A file's part of a stream.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Stream {
    pub blocks: Vec<Block>,
    pub tokens: Vec<Token>,
}

/// A filter of a file at `at` from its start.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Placed {
    pub at: usize,
    pub f: Filter,
}

impl Stream {
    /// The filters of the file, placed, given the tokens' lengths (a stream decoded or checked)
    /// and the last length before it, [`Decoder5::last`].
    pub fn placed(&self, mut last: u32) -> Vec<Placed> {
        let mut pos = 0usize;
        let mut out = Vec::new();
        for t in &self.tokens {
            match *t {
                Token::Lit(_) => pos += 1,
                Token::Copy { len, .. } | Token::Rep { len, .. } => {
                    pos += len as usize;
                    last = len;
                }
                Token::Last => pos += last as usize,
                Token::Filter(f) => out.push(Placed { at: pos.saturating_add(f.start as usize), f }),
            }
        }
        out
    }
}

/// The length of slot `c` (0 to 43) and its extra bits.
#[inline]
fn len_base(c: usize) -> (u32, u32) {
    if c < 8 {
        (2 + c as u32, 0)
    } else {
        let e = c as u32 / 4 - 1;
        (2 + ((4 + (c as u32 & 3)) << e), e)
    }
}

/// The slot of length `n` (2 to 4097).
#[inline]
fn len_slot(n: u32) -> usize {
    if n < 10 {
        (n - 2) as usize
    } else {
        let v = n - 2;
        let e = 31 - v.leading_zeros() - 2;
        (4 * (e + 1) + ((v >> e) - 4)) as usize
    }
}

/// The distance of slot `c` and its extra bits.
#[inline]
fn dist_base(c: usize) -> (u64, u32) {
    if c < 4 {
        (1 + c as u64, 0)
    } else {
        let e = c as u32 / 2 - 1;
        (1 + ((2 + (c as u64 & 1)) << e), e)
    }
}

#[inline]
fn dist_slot(d: u64) -> usize {
    let v = d - 1;
    if v < 4 {
        v as usize
    } else {
        let e = 63 - v.leading_zeros() - 1;
        (2 * (e + 1) + ((v >> e) & 1) as u32) as usize
    }
}

/// The bonus to the length of an explicit copy at `dist`.
#[inline]
pub fn bonus(dist: usize) -> u32 {
    (dist > 0x100) as u32 + (dist > 0x2000) as u32 + (dist > 0x40000) as u32
}

/// Reads up to 64 bits.
fn get_long(r: &mut BitReader, n: u32) -> u64 {
    if n > 32 { (r.get(n - 32) as u64) << 32 | r.get(32) as u64 } else { r.get(n) as u64 }
}

fn put_long(w: &mut BitWriter, v: u64, n: u32) {
    if n > 32 {
        w.put((v >> 32) as u32, n - 32);
        w.put(v as u32, 32);
    } else {
        w.put(v as u32, n);
    }
}

/// The decoders of a block's four codes.
struct Codes([Decoder; 4]);

fn split_lens(lens: &[u8], v1: bool) -> [&[u8]; 4] {
    let (main, rest) = lens.split_at(MAIN);
    let (dist, rest) = rest.split_at(dists(v1));
    let (low, rep) = rest.split_at(LOW);
    [main, dist, low, rep]
}

/// Reads the tables at the start of a block body.
fn read_tables(r: &mut BitReader, end: usize, v1: bool) -> Result<Tables> {
    let mut pre = [0u8; PRE];
    let mut i = 0;
    while i < PRE {
        let v = r.get(4) as u8;
        if v == 15 {
            let z = r.get(4) as usize;
            if z == 0 {
                pre[i] = 15;
                i += 1;
            } else {
                let n = (z + 2).min(PRE - i);
                i += n;
            }
        } else {
            pre[i] = v;
            i += 1;
        }
    }
    let pd = Decoder::new(&pre)?;
    let n = lens_len(v1);
    let mut lens = vec![0u8; n];
    let mut i = 0;
    while i < n {
        if r.pos > end {
            return Err(corrupt());
        }
        let s = pd.read(r)?;
        match s {
            0..=15 => {
                lens[i] = s as u8;
                i += 1;
            }
            16 | 17 => {
                if i == 0 {
                    return Err(corrupt());
                }
                let k = if s == 16 { 3 + r.get(3) } else { 11 + r.get(7) } as usize;
                let v = lens[i - 1];
                let k = k.min(n - i);
                lens[i..i + k].fill(v);
                i += k;
            }
            _ => {
                let k = if s == 18 { 3 + r.get(3) } else { 11 + r.get(7) } as usize;
                i += k.min(n - i);
            }
        }
    }
    if r.pos > end {
        return Err(corrupt());
    }
    Ok(Tables { pre, lens })
}

/// The decoder of a stream, file after file.
pub struct Decoder5 {
    pub v1: bool,
    /// The dictionary: no distance reaches farther.
    pub dict: u64,
    /// The stream's data so far, unfiltered: all its files' in turn.
    pub out: Vec<u8>,
    cache: [usize; 4],
    last: u32,
    codes: Option<Codes>,
    /// The length [`Decoder5::out`] is to have after the file being decoded.
    want: usize,
}

impl Decoder5 {
    pub fn new(v1: bool, dict: u64) -> Self {
        Decoder5 { v1, dict, out: Vec::new(), cache: [0; 4], last: 0, codes: None, want: 0 }
    }

    /// The length of the last copy so far.
    pub fn last(&self) -> u32 {
        self.last
    }

    /// Decodes a file's part `packed` to `size` bytes more of [`Decoder5::out`].
    pub fn file(&mut self, packed: &[u8], size: usize) -> Result<Stream> {
        let base = self.out.len();
        let want = base.checked_add(size).ok_or_else(corrupt)?;
        self.want = want;
        // (a symbol of a bit gives 4100 bytes at most)
        self.out.reserve(size.min(packed.len().saturating_mul(8 * 4100)));
        let buf = padded(packed);
        let mut st = Stream::default();
        let mut at = 0usize;
        loop {
            let h = packed.get(at..at + 3).ok_or_else(corrupt)?;
            let (flags, check) = (h[0], h[1]);
            let k = ((flags >> 3) & 7) as usize + 1;
            if k > 3 {
                return Err(corrupt());
            }
            let sz = packed.get(at + 2..at + 2 + k).ok_or_else(corrupt)?;
            let s = sz.iter().rev().fold(0usize, |a, &b| a << 8 | b as usize);
            let x = 0x5a ^ flags ^ (s as u8) ^ ((s >> 8) as u8) ^ ((s >> 16) as u8);
            if x != check || s == 0 {
                return Err(corrupt());
            }
            let body = at + 2 + k;
            if body + s > packed.len() {
                return Err(corrupt());
            }
            let end = body * 8 + (s - 1) * 8 + (flags & 7) as usize + 1;
            let mut r = BitReader::new(&buf, body * 8);
            let tables = if flags & 0x80 != 0 {
                let t = read_tables(&mut r, end, self.v1)?;
                let [a, b, c, d] = split_lens(&t.lens, self.v1);
                self.codes = Some(Codes([Decoder::new(a)?, Decoder::new(b)?, Decoder::new(c)?, Decoder::new(d)?]));
                Some(t)
            } else {
                None
            };
            let first = st.tokens.len();
            self.symbols(&mut r, end, &mut st.tokens)?;
            let last = flags & 0x40 != 0;
            st.blocks.push(Block { tables, tokens: st.tokens.len() - first, last });
            at = body + s;
            if last {
                break;
            }
        }
        if at != packed.len() || self.out.len() != want {
            return Err(Error("a stream of another length"));
        }
        Ok(st)
    }

    /// Decodes the symbols of a block body up to bit `end`.
    fn symbols(&mut self, r: &mut BitReader, end: usize, tokens: &mut Vec<Token>) -> Result<()> {
        let Some(Codes([main, dist, low, rep])) = &self.codes else { return Err(corrupt()) };
        let out = &mut self.out;
        let want = self.want;
        while r.pos < end {
            let s = main.read(r)? as usize;
            let t = if s < 256 {
                out.push(s as u8);
                Token::Lit(s as u8)
            } else if s == 256 {
                let mut field = || {
                    let m = r.get(2) as u8 + 1;
                    let v = (0..m as u32).fold(0u32, |a, i| a | r.get(8) << (8 * i));
                    (v, m)
                };
                let (start, start_bytes) = field();
                let (len, len_bytes) = field();
                let kind = r.get(3) as u8;
                let channels = if kind == 0 { r.get(5) as u8 + 1 } else { 0 };
                if kind > 3 {
                    return Err(corrupt());
                }
                Token::Filter(Filter { start, len, kind, channels, start_bytes, len_bytes })
            } else if s == 257 {
                if self.last == 0 {
                    return Err(Error("the last copy before any"));
                }
                copy(out, self.cache[0], self.last as usize, want)?;
                Token::Last
            } else if s < 262 {
                let idx = s - 258;
                let d = self.cache[idx];
                self.cache.copy_within(0..idx, 1);
                self.cache[0] = d;
                let c = rep.read(r)? as usize;
                if c >= REPLEN {
                    return Err(corrupt());
                }
                let (b, e) = len_base(c);
                let len = b + r.get(e);
                self.last = len;
                copy(out, d, len as usize, want)?;
                Token::Rep { idx: idx as u8, len }
            } else {
                let (b, e) = len_base(s - 262);
                let mut len = b + r.get(e);
                let c = dist.read(r)? as usize;
                let (b, e) = dist_base(c);
                let x = if e < 4 { r.get(e) as u64 } else { get_long(r, e - 4) << 4 | low.read(r)? as u64 };
                let d = b + x;
                if d > self.dict || d > out.len() as u64 {
                    return Err(Error("a distance past the data"));
                }
                let d = d as usize;
                len += bonus(d);
                self.cache.copy_within(0..3, 1);
                self.cache[0] = d;
                self.last = len;
                copy(out, d, len as usize, want)?;
                Token::Copy { len, dist: d }
            };
            tokens.push(t);
        }
        if r.pos != end {
            return Err(Error("a symbol past the block"));
        }
        Ok(())
    }
}

/// Appends `n` bytes from `d` back.
#[inline]
fn copy(out: &mut Vec<u8>, d: usize, n: usize, want: usize) -> Result<()> {
    if d == 0 || d > out.len() {
        return Err(Error("a distance past the data"));
    }
    if n > want.saturating_sub(out.len()) {
        return Err(Error("a stream of another length"));
    }
    let from = out.len() - d;
    if d >= n {
        out.extend_from_within(from..from + n);
    } else {
        for i in 0..n {
            let b = out[from + i];
            out.push(b);
        }
    }
    Ok(())
}

/// The writer of a stream, file after file (the codes carry over).
pub struct Encoder5 {
    pub v1: bool,
    /// The codes of the last tables (the vectors made once, filled in place), if any were sent.
    codes: [Vec<(u16, u8)>; 4],
    have: bool,
    last: u32,
}

impl Encoder5 {
    pub fn new(v1: bool) -> Self {
        let codes = [MAIN, dists(v1), LOW, REPLEN].map(Vec::with_capacity);
        Encoder5 { v1, codes, have: false, last: 0 }
    }

    /// Writes a file's part `st` onto `out`.
    pub fn file(&mut self, st: &Stream, out: &mut Vec<u8>) -> Result<()> {
        let mut at = 0;
        for b in &st.blocks {
            let tokens = st.tokens.get(at..at + b.tokens).ok_or_else(corrupt)?;
            at += b.tokens;
            self.block(tokens, b.tables.as_ref(), b.last, out)?;
        }
        if at != st.tokens.len() {
            return Err(Error("tokens past the blocks"));
        }
        Ok(())
    }

    /// Writes a block of `tokens` (after `tables` if it sends any) onto the end of `out`, its
    /// body where it is written (the header made room for, then closed up).
    pub fn block(&mut self, tokens: &[Token], tables: Option<&Tables>, last: bool, out: &mut Vec<u8>) -> Result<()> {
        // (flags, check and three bytes of the size, of which the block takes the first one to three)
        let start = out.len();
        out.extend_from_slice(&[0; 5]);
        let base = (start + 5) * 8;
        let mut w = BitWriter::default();
        w.out = std::mem::take(out);
        if let Some(t) = tables {
            write_tables(&mut w, t, self.v1)?;
            let [a, b, c, d] = split_lens(&t.lens, self.v1);
            for (lens, codes) in [a, b, c, d].into_iter().zip(&mut self.codes) {
                huff::codes_into(lens, codes)?;
            }
            self.have = true;
        }
        if !self.have {
            return Err(Error("a block without codes"));
        }
        for t in tokens {
            write_token(&mut w, &self.codes, t, &mut self.last)?;
        }
        let bits = w.len() - base;
        if bits == 0 {
            return Err(Error("an empty block"));
        }
        *out = w.finish();
        let s = out.len() - (start + 5);
        let k = if s < 1 << 8 {
            1
        } else if s < 1 << 16 {
            2
        } else if s < 1 << 24 {
            3
        } else {
            return Err(Error("a block too long"));
        };
        let used = bits - (s - 1) * 8;
        let flags = (used - 1) as u8 | ((k - 1) as u8) << 3 | (last as u8) << 6 | (tables.is_some() as u8) << 7;
        let check = 0x5a ^ flags ^ (s as u8) ^ ((s >> 8) as u8) ^ ((s >> 16) as u8);
        out.copy_within(start + 5.., start + 2 + k);
        out.truncate(out.len() - (3 - k));
        out[start] = flags;
        out[start + 1] = check;
        out[start + 2..start + 2 + k].copy_from_slice(&(s as u32).to_le_bytes()[..k]);
        Ok(())
    }
}

fn write_tables(w: &mut BitWriter, t: &Tables, v1: bool) -> Result<()> {
    if t.lens.len() != lens_len(v1) {
        return Err(corrupt());
    }
    let mut i = 0;
    while i < PRE {
        let l = t.pre[i];
        if l == 0 {
            let run = t.pre[i..].iter().take_while(|&&x| x == 0).count();
            if run >= 3 {
                let k = run.min(17);
                w.put(15, 4);
                w.put(k as u32 - 2, 4);
                i += k;
                continue;
            }
            w.put(0, 4);
        } else if l == 15 {
            w.put(15, 4);
            w.put(0, 4);
        } else {
            w.put(l as u32, 4);
        }
        i += 1;
    }
    let pc = huff::codes(&t.pre)?;
    for (s, extra, n) in rle(&t.lens) {
        huff::write(w, &pc, s as usize)?;
        w.put(extra, n);
    }
    Ok(())
}

/// The code lengths run-length coded as RAR codes them: symbols, their extra bits and their
/// counts.
pub fn rle(lens: &[u8]) -> Vec<(u8, u32, u32)> {
    let mut out = Vec::new();
    let mut i = 0;
    let n = lens.len();
    while i < n {
        let v = lens[i];
        if v == 0 {
            let run = lens[i..].iter().take_while(|&&x| x == 0).count();
            if run >= 3 {
                let k = run.min(138);
                out.push(if k <= 10 { (18, k as u32 - 3, 3) } else { (19, k as u32 - 11, 7) });
                i += k;
                continue;
            }
            out.push((0, 0, 0));
            i += 1;
            continue;
        }
        out.push((v, 0, 0));
        i += 1;
        loop {
            let run = lens[i..].iter().take_while(|&&x| x == v).count();
            if run < 3 {
                break;
            }
            let k = run.min(138);
            out.push(if k <= 10 { (16, k as u32 - 3, 3) } else { (17, k as u32 - 11, 7) });
            i += k;
        }
    }
    out
}

fn write_token(w: &mut BitWriter, codes: &[Vec<(u16, u8)>; 4], t: &Token, last: &mut u32) -> Result<()> {
    let [main, dist, low, rep] = codes;
    match *t {
        Token::Lit(b) => huff::write(w, main, b as usize)?,
        Token::Filter(f) => {
            huff::write(w, main, 256)?;
            for (v, m) in [(f.start, f.start_bytes), (f.len, f.len_bytes)] {
                if !(1..=4).contains(&m) || (m < 4 && v >> (8 * m) != 0) {
                    return Err(Error("a filter's field of other bytes"));
                }
                w.put(m as u32 - 1, 2);
                for i in 0..m as u32 {
                    w.put(v >> (8 * i) & 0xff, 8);
                }
            }
            if f.kind > 3 || (f.kind == 0) != (f.channels != 0) || f.channels > 32 {
                return Err(corrupt());
            }
            w.put(f.kind as u32, 3);
            if f.kind == 0 {
                w.put(f.channels as u32 - 1, 5);
            }
        }
        Token::Last => huff::write(w, main, 257)?,
        Token::Rep { idx, len } => {
            if idx > 3 || !(2..=4097).contains(&len) {
                return Err(corrupt());
            }
            huff::write(w, main, 258 + idx as usize)?;
            let c = len_slot(len);
            huff::write(w, rep, c)?;
            let (b, e) = len_base(c);
            w.put(len - b, e);
            *last = len;
        }
        Token::Copy { len, dist: d } => {
            let n = len.checked_sub(bonus(d)).filter(|n| (2..=4097).contains(n)).ok_or_else(corrupt)?;
            if d == 0 {
                return Err(corrupt());
            }
            let c = len_slot(n);
            huff::write(w, main, 262 + c)?;
            let (b, e) = len_base(c);
            w.put(n - b, e);
            let c = dist_slot(d as u64);
            huff::write(w, dist, c)?;
            let (b, e) = dist_base(c);
            let x = d as u64 - b;
            if e < 4 {
                w.put(x as u32, e);
            } else {
                put_long(w, x >> 4, e - 4);
                huff::write(w, low, (x & 15) as usize)?;
            }
            *last = len;
        }
    }
    Ok(())
}

/// Applies the filters `placed` of a file to its unfiltered data `d`, in place: the file's data.
pub fn unfilter(d: &mut [u8], placed: &[Placed]) -> Result<()> {
    let mut end = 0;
    let mut src = Vec::new();
    for p in placed {
        let (at, len) = (p.at, p.f.len as usize);
        let to = at.checked_add(len).filter(|&to| at >= end && to <= d.len());
        let Some(to) = to else { return Err(Error("a filter out of place")) };
        end = to;
        let dst = &mut d[at..end];
        src.clear();
        src.extend_from_slice(dst);
        match p.f.kind {
            0 => {
                let c = p.f.channels as usize;
                let mut k = 0;
                for ch in 0..c {
                    let mut acc = 0u8;
                    for q in (ch..len).step_by(c) {
                        acc = acc.wrapping_sub(src[k]);
                        dst[q] = acc;
                        k += 1;
                    }
                }
            }
            1 | 2 => {
                let mut i = 0;
                while i + 4 < len {
                    let b = src[i];
                    i += 1;
                    if b == 0xe8 || (p.f.kind == 2 && b == 0xe9) {
                        let pos = ((at + i) as u32) & 0xff_ffff;
                        let a = u32::from_le_bytes(src[i..i + 4].try_into().unwrap());
                        if a >= 1 << 31 {
                            if a.wrapping_add(pos) < 1 << 31 {
                                dst[i..i + 4].copy_from_slice(&a.wrapping_add(1 << 24).to_le_bytes());
                            }
                        } else if a < 1 << 24 {
                            dst[i..i + 4].copy_from_slice(&a.wrapping_sub(pos).to_le_bytes());
                        }
                        i += 4;
                    }
                }
            }
            _ => {
                let mut i = 0;
                while i + 4 <= len {
                    if src[i + 3] == 0xeb {
                        let v = u32::from_le_bytes([src[i], src[i + 1], src[i + 2], 0]);
                        let v = v.wrapping_sub(((at + i) / 4) as u32) & 0xff_ffff;
                        dst[i..i + 3].copy_from_slice(&v.to_le_bytes()[..3]);
                    }
                    i += 4;
                }
            }
        }
    }
    Ok(())
}

/// The symbols token `t` writes, to `f`: of the main code (0), the distance code (1), the low
/// distance code (2), the repeated length code (3).
pub fn symbols(t: &Token, mut f: impl FnMut(usize, usize)) {
    match *t {
        Token::Lit(b) => f(0, b as usize),
        Token::Filter(_) => f(0, 256),
        Token::Last => f(0, 257),
        Token::Rep { idx, len } => {
            f(0, 258 + idx as usize);
            f(3, len_slot(len.max(2)));
        }
        Token::Copy { len, dist } => {
            f(0, 262 + len_slot(len.saturating_sub(bonus(dist)).max(2)));
            let c = dist_slot(dist.max(1) as u64);
            f(1, c);
            let (b, e) = dist_base(c);
            if e >= 4 {
                f(2, ((dist as u64 - b) & 15) as usize);
            }
        }
    }
}

/// The counts of the symbols of `tokens` by code.
pub fn counts(tokens: &[Token], v1: bool) -> [Vec<u32>; 4] {
    let mut c = [vec![0; MAIN], vec![0; dists(v1)], vec![0; LOW], vec![0; REPLEN]];
    for t in tokens {
        symbols(t, |k, s| c[k][s] += 1);
    }
    c
}
