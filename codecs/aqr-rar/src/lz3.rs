//! RAR 2.9 streams (RAR 3.x and 4.x; `docs/rar3-decoding.md` §3–§5, `FORMAT.md`): decoded
//! into their unfiltered data and their blocks as written (LZ blocks' tables and tokens, PPMd
//! blocks' symbols), and written again from those. A solid stream runs on through its files:
//! the history, the codes, the distances, the PPMd model and the filters' programs carry over.

use aqr_common::msb::{BitReader, BitWriter, ZeroReader, padded};

use crate::huff::{self, Decoder};
use crate::ppmd3::{self, Ppmd};
use crate::{Error, Result, corrupt};

pub const MAIN: usize = 299;
pub const DIST: usize = 60;
pub const LOW: usize = 17;
pub const LEN: usize = 28;
/// Code lengths of a table.
pub const LENS: usize = MAIN + DIST + LOW + LEN;
pub const PRE: usize = 20;

/// A token of an LZ block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Token {
    Lit(u8),
    /// The last match again (258).
    Last,
    /// The distance at `idx` of the four last, `len` bytes (259-262).
    Rep {
        idx: u8,
        len: u32,
    },
    /// Two bytes from `dist` back, at most 256 (263-270).
    Short {
        dist: u32,
    },
    /// An explicit match: `len` bytes (with the bonus of its distance) from `dist` back; `run`
    /// where its low distance bits start a run of the last ones (the low symbol 16).
    Copy {
        len: u32,
        dist: u32,
        run: bool,
    },
    /// The filter record of [`Stream::filters`] at this index.
    Filter(u32),
    /// A run of bytes of a PPMd block that are literals, as the rebuilding of a stream keeps them
    /// (the bytes themselves are the data's); no block of a decoded stream has one.
    Lits(u32),
}

/// The tables of an LZ block: whether the previous lengths are kept (the new ones sent as
/// differences), the length code's lengths, the code lengths.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Table {
    pub keep: bool,
    pub pre: [u8; PRE],
    pub lens: [u8; LENS],
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Block {
    /// An LZ block: its table (none where a file goes on with the codes of the one before),
    /// its tokens.
    Lz { table: Option<Box<Table>>, tokens: Vec<Token> },
    /// A PPMd block: its flags, memory and escape (as sent: `mem` where flags & 0x20, `esc`
    /// where flags & 0x40), its symbols as coded (the data with the escapes).
    Ppmd { flags: u8, mem: u8, esc: u8, syms: Vec<u8> },
}

/// A filter record as written (its flags byte, its length's bytes, its body) and what it means.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FilterRec {
    pub raw: Vec<u8>,
    /// Where the filter starts in the file, its length, the filter (`STANDARD`), r0 and r1.
    pub at: usize,
    pub len: u32,
    pub kind: u8,
    pub regs: [u32; 2],
}

/// A file's part of a stream.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Stream {
    pub blocks: Vec<Block>,
    /// The bit after the end of an LZ stream: whether the next file starts with a header.
    pub next_header: bool,
    pub filters: Vec<FilterRec>,
    /// The bytes after the stream's end (after a PPMd stream, a trailer).
    pub tail: Vec<u8>,
}

/// The standard filters, recognised by the length and CRC-32 of their programs.
pub const STANDARD: [(u32, u32); 5] =
    [(29, 0x0e06_077d), (53, 0xad57_6887), (57, 0x3cd7_e57e), (149, 0x1c2c_5dc8), (216, 0xbc85_e701)];
pub const DELTA: u8 = 0;
pub const E8: u8 = 1;
pub const E8E9: u8 = 2;
pub const RGB: u8 = 3;
pub const AUDIO: u8 = 4;

const LEN_BITS: [u8; 28] = [0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5];
const SHORT_BASE: [u32; 8] = [0, 4, 8, 16, 32, 64, 128, 192];
const SHORT_BITS: [u8; 8] = [2, 2, 3, 4, 5, 6, 6, 6];

#[inline]
fn len_base(i: usize) -> u32 {
    if i < 8 { i as u32 } else { (4 + (i as u32 & 3)) << ((i as u32 - 4) / 4) }
}

/// The slot (0 to 27) of the length `v` less 2 or 3, the last for all above it.
#[inline]
fn len_slot(v: u32) -> usize {
    if v < 8 {
        v as usize
    } else {
        let e = 31 - v.leading_zeros() - 2;
        ((4 * (e + 1) + (v >> e) - 4) as usize).min(27)
    }
}

#[inline]
fn dist_bits(s: usize) -> u32 {
    match s {
        0..=3 => 0,
        4..=35 => s as u32 / 2 - 1,
        36..=47 => 16,
        _ => 18,
    }
}

/// The bases of the distance slots.
fn dist_bases() -> [u32; DIST] {
    let mut b = [0u32; DIST];
    for s in 1..DIST {
        b[s] = b[s - 1] + (1 << dist_bits(s - 1));
    }
    b
}

#[inline]
fn dist_slot(bases: &[u32; DIST], d: u32) -> usize {
    bases.partition_point(|&b| b < d) - 1
}

/// The bonus to the length of an explicit match at `dist`.
#[inline]
pub fn bonus(dist: u32) -> u32 {
    (dist >= 0x2000) as u32 + (dist >= 0x40000) as u32
}

/// The most programs a stream defines (RAR defines each of the five once).
pub const MAX_PROGS: usize = 16;

/// A program of the filters' state: its filter, its last length.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Prog {
    pub kind: u8,
    pub len: u32,
}

/// What a filter record of `raw` means given the programs `progs` and the last one used
/// `last`, at output position `p`: its start (in the stream), length, filter and registers.
/// The programs' state is updated.
pub(crate) fn parse_filter(
    raw: &[u8],
    progs: &mut Vec<Prog>,
    last: &mut usize,
    p: u64,
) -> Result<(u64, u32, u8, [u32; 2])> {
    let &g = raw.first().ok_or_else(corrupt)?;
    let (skip, len) = match g & 7 {
        6 => (2, *raw.get(1).ok_or_else(corrupt)? as usize + 7),
        7 => (3, (*raw.get(1).ok_or_else(corrupt)? as usize) << 8 | *raw.get(2).ok_or_else(corrupt)? as usize),
        k => (1, k as usize + 1),
    };
    if raw.len() != skip + len {
        return Err(corrupt());
    }
    let n = (raw.len() - skip) * 8;
    let mut r = ZeroReader::new(&raw[skip..], 0);
    let num = |r: &mut ZeroReader| -> Result<u32> {
        let v = match r.get(2) {
            0 => r.get(4),
            1 => {
                let v = r.get(8);
                if v < 16 { 0xffff_ff00 | v << 4 | r.get(4) } else { v }
            }
            2 => r.get(16),
            _ => r.get(32),
        };
        if r.pos > n { Err(corrupt()) } else { Ok(v) }
    };
    let idx = if g & 0x80 != 0 {
        let k = num(&mut r)? as usize;
        if k == 0 {
            progs.clear();
            0
        } else {
            k - 1
        }
    } else {
        *last
    };
    if idx > progs.len() {
        return Err(corrupt());
    }
    let new = idx == progs.len();
    let s = num(&mut r)? as u64;
    let start = p + s + if g & 0x40 != 0 { 258 } else { 0 };
    let len = if g & 0x20 != 0 {
        num(&mut r)?
    } else if new {
        0
    } else {
        progs[idx].len
    };
    let mut regs = [0u32; 7];
    if g & 0x10 != 0 {
        let m = r.get(7);
        for (i, reg) in regs.iter_mut().enumerate() {
            if m >> i & 1 != 0 {
                *reg = num(&mut r)?;
            }
        }
    }
    if new {
        let c = num(&mut r)? as usize;
        if c == 0 || c > 0x10000 || r.pos + 8 * c > n {
            return Err(corrupt());
        }
        // (the program's CRC-32 as its bytes are read: no copy of them, no tables)
        let mut crc = !0u32;
        for _ in 0..c {
            crc ^= r.get(8);
            for _ in 0..8 {
                crc = (crc >> 1) ^ (0xedb8_8320 & (crc & 1).wrapping_neg());
            }
        }
        let kind =
            STANDARD.iter().position(|&s| s == (c as u32, !crc)).ok_or(Error("a filter of no standard program"))?;
        if progs.len() == MAX_PROGS {
            return Err(Error("more programs than the format keeps"));
        }
        progs.push(Prog { kind: kind as u8, len: 0 });
    }
    if g & 0x08 != 0 {
        let k = num(&mut r)? as usize;
        if k > 0x2000 - 0x40 || r.pos + 8 * k > n {
            return Err(corrupt());
        }
        r.skip(8 * k as u32);
    }
    if r.pos > n {
        return Err(corrupt());
    }
    progs[idx].len = len;
    *last = idx;
    Ok((start, len, progs[idx].kind, [regs[0], regs[1]]))
}

/// The decoder of a stream, file after file.
pub struct Decoder3 {
    /// The window: no distance reaches farther.
    pub dict: u64,
    /// The stream's data so far, unfiltered: all its files' in turn.
    pub out: Vec<u8>,
    rep: [u32; 4],
    last: (u32, u32),
    low: (u32, u32),
    prev: [u8; LENS],
    codes: Option<[Decoder; 4]>,
    header: bool,
    ppmd: Option<Ppmd>,
    esc: u8,
    progs: Vec<Prog>,
    last_prog: usize,
    /// Whether PPMd blocks are decoded (else they fail at once).
    pub ppmd_on: bool,
    /// The length [`Decoder3::out`] is to have after the file being decoded.
    want: usize,
}

impl Decoder3 {
    pub fn new(dict: u64) -> Self {
        Decoder3 {
            dict,
            out: Vec::new(),
            rep: [0; 4],
            last: (0, 0),
            low: (0, 0),
            prev: [0; LENS],
            codes: None,
            header: true,
            ppmd: None,
            esc: 2,
            progs: Vec::new(),
            last_prog: 0,
            ppmd_on: true,
            want: 0,
        }
    }

    /// Decodes a file's part `packed` to `size` bytes more of [`Decoder3::out`].
    pub fn file(&mut self, packed: &[u8], size: usize) -> Result<Stream> {
        let base = self.out.len();
        let want = base.checked_add(size).ok_or_else(corrupt)?;
        self.want = want;
        // (a symbol of a bit gives 4100 bytes at most)
        self.out.reserve(size.min(packed.len().saturating_mul(8 * 4100)));
        let buf = padded(packed);
        let mut r = BitReader::new(&buf, 0);
        let mut st = Stream::default();
        let end = packed.len() * 8;
        // (the first block: a header, or the codes of the file before going on)
        let mut header = self.header;
        loop {
            if header {
                r.pos = r.pos.next_multiple_of(8);
                if r.pos + 1 > end {
                    return Err(corrupt());
                }
                if r.get(1) == 1 {
                    if !self.ppmd_on {
                        return Err(Error("PPMd blocks not taken"));
                    }
                    let flags = r.get(7) as u8;
                    let mem = if flags & 0x20 != 0 { r.get(8) as u8 } else { 0 };
                    let esc = if flags & 0x40 != 0 { r.get(8) as u8 } else { 0 };
                    if r.pos > end {
                        return Err(corrupt());
                    }
                    if flags & 0x40 != 0 {
                        self.esc = esc;
                    }
                    if flags & 0x20 != 0 {
                        let order = order(flags)?;
                        ppmd3::start(&mut self.ppmd, order, (mem as u32 + 1) << 20)?;
                    } else if self.ppmd.is_none() {
                        return Err(corrupt());
                    }
                    let at = r.pos / 8;
                    let (syms, used, ending) = self.ppmd_block(&packed[at..], base, &mut st.filters)?;
                    st.blocks.push(Block::Ppmd { flags, mem, esc, syms });
                    r.pos = (at + used) * 8;
                    if ending {
                        // (after the end of a PPMd stream, the next file starts with a header)
                        self.header = true;
                        st.next_header = true;
                        st.tail = packed[r.pos / 8..].to_vec();
                        break;
                    }
                    continue;
                }
                self.table(&mut r, end, &mut st)?;
            } else {
                if self.codes.is_none() {
                    return Err(corrupt());
                }
                st.blocks.push(Block::Lz { table: None, tokens: Vec::new() });
            }
            match self.lz(&mut r, end, base, &mut st)? {
                Some(next) => {
                    self.header = next;
                    st.next_header = next;
                    let at = r.pos.next_multiple_of(8);
                    // (the padding is part of the stream; its bits are checked by writing again)
                    st.tail = packed[at / 8..].to_vec();
                    break;
                }
                None => header = true,
            }
        }
        if self.out.len() != want {
            return Err(Error("a stream of another length"));
        }
        // the filters, placed in the file
        for f in &st.filters {
            if f.at.checked_add(f.len as usize).is_none_or(|to| to > size) {
                return Err(Error("a filter past the file"));
            }
        }
        Ok(st)
    }

    /// Reads an LZ block's table.
    fn table(&mut self, r: &mut BitReader, end: usize, st: &mut Stream) -> Result<()> {
        self.low = (0, 0);
        let keep = r.get(1) == 1;
        if !keep {
            self.prev = [0; LENS];
        }
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
                    i += (z + 2).min(PRE - i);
                }
            } else {
                pre[i] = v;
                i += 1;
            }
        }
        let pd = Decoder::new(&pre)?;
        let mut lens = [0u8; LENS];
        let mut i = 0;
        while i < LENS {
            if r.pos > end {
                return Err(corrupt());
            }
            let s = pd.read(r)?;
            match s {
                0..=15 => {
                    lens[i] = (self.prev[i] + s as u8) & 15;
                    i += 1;
                }
                16 | 17 => {
                    if i == 0 {
                        return Err(corrupt());
                    }
                    let k = if s == 16 { 3 + r.get(3) } else { 11 + r.get(7) } as usize;
                    let k = k.min(LENS - i);
                    let v = lens[i - 1];
                    lens[i..i + k].fill(v);
                    i += k;
                }
                _ => {
                    let k = if s == 18 { 3 + r.get(3) } else { 11 + r.get(7) } as usize;
                    i += k.min(LENS - i);
                }
            }
        }
        if r.pos > end {
            return Err(corrupt());
        }
        self.prev = lens;
        let (a, rest) = lens.split_at(MAIN);
        let (b, rest) = rest.split_at(DIST);
        let (c, d) = rest.split_at(LOW);
        self.codes = Some([Decoder::new(a)?, Decoder::new(b)?, Decoder::new(c)?, Decoder::new(d)?]);
        st.blocks.push(Block::Lz { table: Some(Box::new(Table { keep, pre, lens })), tokens: Vec::new() });
        Ok(())
    }

    /// Decodes an LZ block's symbols onto the last block of `st`, up to its end: Some(whether
    /// the next file starts with a header) at the end of the file, None where a block follows.
    fn lz(&mut self, r: &mut BitReader, end: usize, base: usize, st: &mut Stream) -> Result<Option<bool>> {
        let bases = dist_bases();
        let Some([main, dist, low, len]) = &self.codes else { return Err(corrupt()) };
        let Some(Block::Lz { tokens, .. }) = st.blocks.last_mut() else { unreachable!() };
        let out = &mut self.out;
        loop {
            if r.pos > end {
                return Err(corrupt());
            }
            let s = main.read(r)? as usize;
            let t = match s {
                0..=255 => {
                    out.push(s as u8);
                    Token::Lit(s as u8)
                }
                256 => {
                    let more = r.get(1) == 1;
                    if more {
                        return Ok(None);
                    }
                    let next = r.get(1) == 1;
                    if r.pos > end {
                        return Err(corrupt());
                    }
                    return Ok(Some(next));
                }
                257 => {
                    let mut raw = vec![r.get(8) as u8];
                    let n = match raw[0] & 7 {
                        6 => {
                            let b = r.get(8);
                            raw.push(b as u8);
                            b as usize + 7
                        }
                        7 => {
                            let (a, b) = (r.get(8), r.get(8));
                            raw.extend([a as u8, b as u8]);
                            (a << 8 | b) as usize
                        }
                        k => k as usize + 1,
                    };
                    if r.pos + 8 * n > end {
                        return Err(corrupt());
                    }
                    raw.extend((0..n).map(|_| r.get(8) as u8));
                    let (start, flen, kind, regs) =
                        parse_filter(&raw, &mut self.progs, &mut self.last_prog, out.len() as u64)?;
                    let at =
                        usize::try_from(start.checked_sub(base as u64).ok_or_else(corrupt)?).map_err(|_| corrupt())?;
                    st.filters.push(FilterRec { raw, at, len: flen, kind, regs });
                    Token::Filter(st.filters.len() as u32 - 1)
                }
                258 => {
                    if self.last.1 == 0 {
                        return Err(Error("the last match before any"));
                    }
                    copy(out, self.last.0 as usize, self.last.1 as usize, self.dict, self.want)?;
                    Token::Last
                }
                259..=262 => {
                    let k = s - 259;
                    let d = self.rep[k];
                    self.rep.copy_within(0..k, 1);
                    self.rep[0] = d;
                    let i = len.read(r)? as usize;
                    if i >= LEN {
                        return Err(corrupt());
                    }
                    let l = len_base(i) + 2 + r.get(LEN_BITS[i] as u32);
                    self.last = (d, l);
                    copy(out, d as usize, l as usize, self.dict, self.want)?;
                    Token::Rep { idx: k as u8, len: l }
                }
                263..=270 => {
                    let i = s - 263;
                    let d = SHORT_BASE[i] + 1 + r.get(SHORT_BITS[i] as u32);
                    self.rep.copy_within(0..3, 1);
                    self.rep[0] = d;
                    self.last = (d, 2);
                    copy(out, d as usize, 2, self.dict, self.want)?;
                    Token::Short { dist: d }
                }
                _ => {
                    let i = s - 271;
                    let mut l = len_base(i) + 3 + r.get(LEN_BITS[i] as u32);
                    let ds = dist.read(r)? as usize;
                    if ds >= DIST {
                        return Err(corrupt());
                    }
                    let b = dist_bits(ds);
                    let mut run = false;
                    let x = if ds <= 9 {
                        r.get(b)
                    } else {
                        let high = r.get(b - 4);
                        let lo = if self.low.1 > 0 {
                            self.low.1 -= 1;
                            self.low.0
                        } else {
                            let q = low.read(r)? as u32;
                            if q == 16 {
                                run = true;
                                self.low.1 = 15;
                                self.low.0
                            } else {
                                self.low.0 = q;
                                q
                            }
                        };
                        high << 4 | lo
                    };
                    let d = bases[ds] + 1 + x;
                    l += bonus(d);
                    self.rep.copy_within(0..3, 1);
                    self.rep[0] = d;
                    self.last = (d, l);
                    copy(out, d as usize, l as usize, self.dict, self.want)?;
                    Token::Copy { len: l, dist: d, run }
                }
            };
            tokens.push(t);
        }
    }

    /// Decodes a PPMd block from the start of `d`: its symbols, the bytes its range coder read,
    /// whether it ends the file.
    fn ppmd_block(&mut self, d: &[u8], base: usize, filters: &mut Vec<FilterRec>) -> Result<(Vec<u8>, usize, bool)> {
        let model = self.ppmd.as_mut().ok_or_else(corrupt)?;
        let mut rc = ppmd3::Reader::new(d);
        let mut syms = Vec::new();
        let out = &mut self.out;
        let esc = self.esc;
        let mut get = |rc: &mut ppmd3::Reader, syms: &mut Vec<u8>| -> Result<u8> {
            let s = ppmd3::decode(model, rc)?;
            syms.push(s);
            Ok(s)
        };
        loop {
            let s = get(&mut rc, &mut syms)?;
            if s != esc {
                out.push(s);
                continue;
            }
            match get(&mut rc, &mut syms)? {
                0 => return Ok((syms, rc.used()?, false)),
                1 => out.push(esc),
                2 => return Ok((syms, rc.used()?, true)),
                3 => {
                    // a filter record, its bytes as symbols
                    let mut raw = vec![get(&mut rc, &mut syms)?];
                    let n = match raw[0] & 7 {
                        6 => {
                            let b = get(&mut rc, &mut syms)?;
                            raw.push(b);
                            b as usize + 7
                        }
                        7 => {
                            let (a, b) = (get(&mut rc, &mut syms)?, get(&mut rc, &mut syms)?);
                            raw.extend([a, b]);
                            (a as usize) << 8 | b as usize
                        }
                        k => k as usize + 1,
                    };
                    for _ in 0..n {
                        let b = get(&mut rc, &mut syms)?;
                        raw.push(b);
                    }
                    let (start, len, kind, regs) =
                        parse_filter(&raw, &mut self.progs, &mut self.last_prog, out.len() as u64)?;
                    let at =
                        usize::try_from(start.checked_sub(base as u64).ok_or_else(corrupt)?).map_err(|_| corrupt())?;
                    filters.push(FilterRec { raw, at, len, kind, regs });
                }
                4 => {
                    let mut v = 0u32;
                    for _ in 0..3 {
                        v = v << 8 | get(&mut rc, &mut syms)? as u32;
                    }
                    let l = get(&mut rc, &mut syms)? as usize + 32;
                    copy(out, v as usize + 2, l, self.dict, self.want)?;
                }
                5 => {
                    let l = get(&mut rc, &mut syms)? as usize + 4;
                    copy(out, 1, l, self.dict, self.want)?;
                }
                _ => return Err(Error("a PPMd escape of no known meaning")),
            }
        }
    }
}

/// The order of a PPMd block of `flags`.
fn order(flags: u8) -> Result<u32> {
    let o = (flags & 0x1f) as u32 + 1;
    let o = if o > 16 { 16 + 3 * (o - 16) } else { o };
    if o < 2 { Err(corrupt()) } else { Ok(o) }
}

/// Appends `n` bytes from `d` back.
#[inline]
fn copy(out: &mut Vec<u8>, d: usize, n: usize, dict: u64, want: usize) -> Result<()> {
    if d == 0 || d > out.len() || d as u64 > dict {
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

/// The writer of a stream, file after file.
pub struct Encoder3 {
    prev: [u8; LENS],
    bases: [u32; DIST],
    /// The codes of the last table (the vectors made once, filled in place), if any were sent.
    codes: [Vec<(u16, u8)>; 4],
    have: bool,
    low: (u32, u32),
    ppmd: Option<Ppmd>,
}

impl Default for Encoder3 {
    fn default() -> Self {
        Self::new()
    }
}

impl Encoder3 {
    pub fn new() -> Self {
        let codes = [MAIN, DIST, LOW, LEN].map(Vec::with_capacity);
        Encoder3 { prev: [0; LENS], bases: dist_bases(), codes, have: false, low: (0, 0), ppmd: None }
    }

    /// A writer whose PPMd model holds `mib` MiB of memory, as much as any of the stream's blocks
    /// uses (none where `mib` is 0): the model is made at once, and never again.
    pub fn with_model(mib: u32) -> Result<Self> {
        let mut e = Self::new();
        if mib != 0 {
            e.ppmd = Some(ppmd3::with_capacity(mib << 20)?);
        }
        Ok(e)
    }

    /// The code lengths of the last table written (zeros before any).
    pub fn prev(&self) -> &[u8; LENS] {
        &self.prev
    }

    /// Writes a file's part `st` onto `out`; `header` whether its first block has a header (the
    /// last file's `next_header`; true for a stream's first file).
    pub fn file(&mut self, st: &Stream, header: bool, out: &mut Vec<u8>) -> Result<()> {
        let mut w = BitWriter::default();
        w.out = std::mem::take(out);
        let n = st.blocks.len();
        let raw = |i: u32| st.filters.get(i as usize).map(|f| &f.raw[..]);
        for (bi, b) in st.blocks.iter().enumerate() {
            let last = bi + 1 == n;
            match b {
                Block::Lz { table, tokens } => {
                    let end = last.then_some(st.next_header);
                    self.lz_block(&mut w, table.as_deref(), bi == 0 && !header, tokens, &raw, end)?
                }
                Block::Ppmd { flags, mem, esc, syms } => {
                    if bi == 0 && !header {
                        return Err(Error("a PPMd block without a header"));
                    }
                    self.ppmd_block(&mut w, *flags, *mem, *esc, |put| {
                        syms.iter().for_each(|&s| put(s));
                        Ok(())
                    })?
                }
            }
        }
        *out = w.finish();
        out.extend_from_slice(&st.tail);
        Ok(())
    }

    /// Writes an LZ block onto `w` (whose output is the stream's): after `table` if it has one
    /// (else `no_table_ok`: a file going on with the codes of the one before), its `tokens`
    /// (`raw` the bytes of the filter records they name), and then the end of the file's part
    /// (`end` the bit after it) or of the block.
    pub fn lz_block<'r>(
        &mut self,
        w: &mut BitWriter,
        table: Option<&Table>,
        no_table_ok: bool,
        tokens: &[Token],
        raw: &dyn Fn(u32) -> Option<&'r [u8]>,
        end: Option<bool>,
    ) -> Result<()> {
        match table {
            Some(t) => {
                align(w);
                w.put(0, 1);
                self.write_table(w, t)?;
            }
            None if no_table_ok => {}
            None => return Err(Error("an LZ block without a table")),
        }
        if !self.have {
            return Err(Error("a block without codes"));
        }
        for t in tokens {
            write_token(w, &self.codes, &self.bases, &mut self.low, raw, t)?;
        }
        huff::write(w, &self.codes[0], 256)?;
        match end {
            Some(next_header) => {
                w.put(0, 1);
                w.put(next_header as u32, 1);
            }
            None => w.put(1, 1),
        }
        Ok(())
    }

    /// Writes a PPMd block onto `w`: its header, the model started where `flags` say, and the
    /// symbols that `feed` gives to the function it is called with, coded; then `w` is at a byte
    /// boundary.
    pub fn ppmd_block(
        &mut self,
        w: &mut BitWriter,
        flags: u8,
        mem: u8,
        esc: u8,
        feed: impl FnOnce(&mut dyn FnMut(u8)) -> Result<()>,
    ) -> Result<()> {
        align(w);
        w.put(1, 1);
        w.put(flags as u32, 7);
        if flags & 0x20 != 0 {
            w.put(mem as u32, 8);
            ppmd3::start(&mut self.ppmd, order(flags)?, (mem as u32 + 1) << 20)?;
        } else if self.ppmd.is_none() {
            return Err(corrupt());
        }
        if flags & 0x40 != 0 {
            w.put(esc as u32, 8);
        }
        let out = std::mem::take(w).finish();
        let model = self.ppmd.as_mut().ok_or_else(corrupt)?;
        let mut rc = ppmd3::Writer::with_output(out);
        feed(&mut |s| ppmd3::encode(model, &mut rc, s))?;
        w.out = rc.finish();
        Ok(())
    }

    fn write_table(&mut self, w: &mut BitWriter, t: &Table) -> Result<()> {
        self.low = (0, 0);
        w.put(t.keep as u32, 1);
        if !t.keep {
            self.prev = [0; LENS];
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
        for (s, extra, n) in rle(&t.lens, &self.prev) {
            huff::write(w, &pc, s as usize)?;
            w.put(extra, n);
        }
        self.prev = t.lens;
        let (a, rest) = t.lens.split_at(MAIN);
        let (b, rest) = rest.split_at(DIST);
        let (c, d) = rest.split_at(LOW);
        for (lens, codes) in [a, b, c, d].into_iter().zip(&mut self.codes) {
            huff::codes_into(lens, codes)?;
        }
        self.have = true;
        Ok(())
    }
}

/// Pads `w` with zero bits to a byte boundary.
fn align(w: &mut BitWriter) {
    let k = (8 - w.len() % 8) % 8;
    w.put(0, k as u32);
}

/// The code lengths `lens` run-length coded as RAR codes them against the previous ones
/// `prev`: symbols, their extra bits and their counts.
pub fn rle(lens: &[u8; LENS], prev: &[u8; LENS]) -> Vec<(u8, u32, u32)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < LENS {
        let v = lens[i];
        if v == 0 {
            let run = lens[i..].iter().take_while(|&&x| x == 0).count();
            if run >= 3 {
                let k = run.min(138);
                out.push(if k <= 10 { (18, k as u32 - 3, 3) } else { (19, k as u32 - 11, 7) });
                i += k;
                continue;
            }
        } else if i > 0 && lens[i - 1] == v {
            let run = lens[i..].iter().take_while(|&&x| x == v).count();
            if run >= 3 {
                let k = run.min(138);
                out.push(if k <= 10 { (16, k as u32 - 3, 3) } else { (17, k as u32 - 11, 7) });
                i += k;
                continue;
            }
        }
        out.push(((v.wrapping_sub(prev[i])) & 15, 0, 0));
        i += 1;
    }
    out
}

fn write_token<'r>(
    w: &mut BitWriter,
    codes: &[Vec<(u16, u8)>; 4],
    bases: &[u32; DIST],
    low: &mut (u32, u32),
    raw: &dyn Fn(u32) -> Option<&'r [u8]>,
    t: &Token,
) -> Result<()> {
    let [main, dist, lowc, len] = codes;
    match *t {
        Token::Lit(b) => huff::write(w, main, b as usize)?,
        Token::Filter(i) => {
            huff::write(w, main, 257)?;
            for &b in raw(i).ok_or_else(corrupt)? {
                w.put(b as u32, 8);
            }
        }
        Token::Lits(_) => return Err(corrupt()),
        Token::Last => huff::write(w, main, 258)?,
        Token::Rep { idx, len: l } => {
            if idx > 3 || !(2..=257).contains(&l) {
                return Err(corrupt());
            }
            huff::write(w, main, 259 + idx as usize)?;
            let i = len_slot(l - 2);
            huff::write(w, len, i)?;
            w.put(l - 2 - len_base(i), LEN_BITS[i] as u32);
        }
        Token::Short { dist: d } => {
            if !(1..=256).contains(&d) {
                return Err(corrupt());
            }
            let i = SHORT_BASE.iter().rposition(|&b| b < d).unwrap();
            huff::write(w, main, 263 + i)?;
            w.put(d - 1 - SHORT_BASE[i], SHORT_BITS[i] as u32);
        }
        Token::Copy { len: l, dist: d, run } => {
            let n = l.checked_sub(bonus(d) + 3).filter(|&n| n <= 255).ok_or_else(corrupt)?;
            if d == 0 {
                return Err(corrupt());
            }
            let i = len_slot(n);
            huff::write(w, main, 271 + i)?;
            w.put(n - len_base(i), LEN_BITS[i] as u32);
            let s = dist_slot(bases, d);
            huff::write(w, dist, s)?;
            let x = d - 1 - bases[s];
            let b = dist_bits(s);
            if s <= 9 {
                if run {
                    return Err(corrupt());
                }
                w.put(x, b);
            } else {
                w.put(x >> 4, b - 4);
                let lo = x & 15;
                if low.1 > 0 {
                    if run || lo != low.0 {
                        return Err(Error("low distance bits not of the run"));
                    }
                    low.1 -= 1;
                } else if run {
                    if lo != low.0 {
                        return Err(corrupt());
                    }
                    huff::write(w, lowc, 16)?;
                    low.1 = 15;
                } else {
                    huff::write(w, lowc, lo as usize)?;
                    low.0 = lo;
                }
            }
        }
    }
    Ok(())
}

/// The symbols token `t` writes, to `f`: of the main code (0), the distance code (1), the low
/// distance code (2), the length code (3); `low` the low distance bits' run as it stands.
pub fn symbols(t: &Token, bases: &[u32; DIST], low: &mut (u32, u32), mut f: impl FnMut(usize, usize)) {
    match *t {
        Token::Lit(b) => f(0, b as usize),
        Token::Filter(_) => f(0, 257),
        Token::Lits(_) => {}
        Token::Last => f(0, 258),
        Token::Rep { idx, len } => {
            f(0, 259 + idx as usize);
            f(3, len_slot(len.max(2) - 2));
        }
        Token::Short { dist } => f(0, 263 + SHORT_BASE.iter().rposition(|&b| b < dist.max(1)).unwrap()),
        Token::Copy { len, dist, run } => {
            let n = len.saturating_sub(bonus(dist) + 3).min(255);
            f(0, 271 + len_slot(n));
            let s = dist_slot(bases, dist.max(1));
            f(1, s);
            if s > 9 {
                let lo = (dist.max(1) - 1 - bases[s]) & 15;
                if low.1 > 0 {
                    low.1 -= 1;
                } else if run {
                    f(2, 16);
                    low.1 = 15;
                } else {
                    f(2, lo as usize);
                    low.0 = lo;
                }
            }
        }
    }
}

/// The distance slots' bases.
pub fn bases() -> [u32; DIST] {
    dist_bases()
}

/// Applies the filters of a file to its unfiltered data `d`, in place: the file's data.
pub fn unfilter(d: &mut [u8], filters: &[FilterRec]) -> Result<()> {
    let mut end = 0;
    let mut src = Vec::new();
    for f in filters {
        let (at, len) = (f.at, f.len as usize);
        let to = at.checked_add(len).filter(|&to| at >= end && to <= d.len());
        let Some(to) = to else { return Err(Error("a filter out of place")) };
        end = to;
        let dst = &mut d[at..end];
        src.clear();
        src.extend_from_slice(dst);
        match f.kind {
            DELTA => {
                let c = f.regs[0] as usize;
                if !(1..=128).contains(&c) {
                    return Err(corrupt());
                }
                let mut k = 0;
                for ch in 0..c {
                    let mut p = 0u8;
                    for j in (ch..len).step_by(c) {
                        p = p.wrapping_sub(src[k]);
                        dst[j] = p;
                        k += 1;
                    }
                }
            }
            E8 | E8E9 => {
                let mut i = 0;
                while i + 5 <= len {
                    if src[i] == 0xe8 || (f.kind == E8E9 && src[i] == 0xe9) {
                        let a = i32::from_le_bytes(src[i + 1..i + 5].try_into().unwrap());
                        let c = (at as u32).wrapping_add(i as u32 + 1);
                        let v = if a < 0 && c >= a.unsigned_abs() {
                            Some(a.wrapping_add(1 << 24))
                        } else if (0..1 << 24).contains(&a) {
                            Some(a.wrapping_sub(c as i32))
                        } else {
                            None
                        };
                        if let Some(v) = v {
                            dst[i + 1..i + 5].copy_from_slice(&v.to_le_bytes());
                        }
                        i += 5;
                    } else {
                        i += 1;
                    }
                }
            }
            RGB => {
                let (s, b) = (f.regs[0] as usize, f.regs[1] as usize);
                if len < 3 || s > len || s < 3 || b > 2 {
                    return Err(corrupt());
                }
                let mut k = 0;
                for c in 0..3 {
                    let mut p = 0u8;
                    for j in (c..len).step_by(3) {
                        if j >= s {
                            let up = dst[j - s + 3] as i32;
                            let ul = dst[j - s] as i32;
                            let pv = p as i32;
                            let d1 = (up - ul).abs();
                            let d2 = (pv - ul).abs();
                            let d3 = (up - ul + pv - ul).abs();
                            if d1 > d2 || d1 > d3 {
                                p = if d2 <= d3 { up as u8 } else { ul as u8 };
                            }
                        }
                        p = p.wrapping_sub(src[k]);
                        k += 1;
                        dst[j] = p;
                    }
                }
                let mut i = b;
                while i + 2 < len {
                    let g = dst[i + 1];
                    dst[i] = dst[i].wrapping_add(g);
                    dst[i + 2] = dst[i + 2].wrapping_add(g);
                    i += 3;
                }
            }
            AUDIO => {
                let c = f.regs[0] as usize;
                if !(1..=128).contains(&c) {
                    return Err(corrupt());
                }
                let mut k = 0;
                for ch in 0..c {
                    let mut a = Audio::default();
                    for j in (ch..len).step_by(c) {
                        dst[j] = a.decode(src[k]);
                        k += 1;
                    }
                }
            }
            _ => return Err(Error("a filter of no known kind")),
        }
    }
    Ok(())
}

/// The audio filter's predictor of a channel.
#[derive(Default)]
pub struct Audio {
    w: [i32; 3],
    /// D1, D2, D3: the last deltas.
    d: [i32; 3],
    last: i32,
    delta: i32,
    e: [u32; 7],
    count: u32,
}

impl Audio {
    /// The prediction of the next sample (the deltas moved on).
    #[inline]
    fn predict(&mut self) -> u8 {
        self.d = [self.delta, self.delta - self.d[0], self.d[1]];
        let [w1, w2, w3] = self.w;
        let [d1, d2, d3] = self.d;
        ((8 * self.last + w1 * d1 + w2 * d2 + w3 * d3) >> 3) as u8
    }

    /// The sample of the coded byte `x`.
    #[inline]
    pub fn decode(&mut self, x: u8) -> u8 {
        let v = self.predict().wrapping_sub(x);
        self.update(x, v);
        v
    }

    /// The coded byte of the sample `v`.
    #[inline]
    pub fn encode(&mut self, v: u8) -> u8 {
        let x = self.predict().wrapping_sub(v);
        self.update(x, v);
        x
    }

    #[inline]
    fn update(&mut self, x: u8, v: u8) {
        let e = 8 * (x as i8 as i32);
        let [d1, d2, d3] = self.d;
        let errs = [e, e - d1, e + d1, e - d2, e + d2, e - d3, e + d3];
        for (a, b) in self.e.iter_mut().zip(errs) {
            *a += b.unsigned_abs();
        }
        self.delta = v.wrapping_sub(self.last as u8) as i8 as i32;
        self.last = v as i32;
        if self.count.is_multiple_of(32) {
            let mut k = 0;
            for i in 1..7 {
                if self.e[i] < self.e[k] {
                    k = i;
                }
            }
            self.e = [0; 7];
            match k {
                1 if self.w[0] >= -16 => self.w[0] -= 1,
                2 if self.w[0] < 16 => self.w[0] += 1,
                3 if self.w[1] >= -16 => self.w[1] -= 1,
                4 if self.w[1] < 16 => self.w[1] += 1,
                5 if self.w[2] >= -16 => self.w[2] -= 1,
                6 if self.w[2] < 16 => self.w[2] += 1,
                _ => {}
            }
        }
        self.count += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn len_slot_inverts_len_base() {
        for v in 0..1000 {
            assert_eq!(len_slot(v), (0..28).rev().find(|&i| len_base(i) <= v).unwrap(), "{v}");
        }
    }
}
