//! The record of a RAR 2.9 stream (RAR 3.x and 4.x) given its data (`FORMAT.md`, "The RAR 3.x
//! record"): a solid group's files (or a file alone), their regions of LZ and PPMd blocks, their
//! filters, the parse, the LZ blocks and their tables.
//!
//! As for RAR5 (`recons5`), the unfiltered data is the contents filtered again, corrected; LZ
//! regions' matches are coded against the four last distances and the longest matches hash
//! chains find, how each is written against RAR's rule. PPMd regions code only where their
//! matches are (the escapes 4 and 5); their symbols follow from the data. Filter records are
//! predicted from what they mean and the programs before them, the tables by Huffman's
//! construction over the symbols they serve.

use std::ops::Range;

use aqr_common::arith::{ArithDecoder, Bit};
use aqr_common::msb::BitWriter;

use crate::huff;
use crate::lz3::{self, Block, Decoder3, Encoder3, FilterRec, Prog, Stream, Table, Token, bonus};
use crate::rec::{Bits, Cat, Dec, Enc, Mf, bits, lcp_up};
use crate::{Error, Result, corrupt};

/// The longest match RAR writes.
const LONG: usize = 257;
/// The longest of the format: 258, with the bonus of the farthest distances.
const CAP: usize = 260;
/// Symbols of an LZ block, RAR's buffer.
const SYMBOLS: usize = 16384;

/// The programs of the standard filters as RAR writes them (`docs/rar3-decoding.md` §5.3).
const PROGRAMS: [&str; 5] = [
    "2f019a4180ec27482f09766dd3ea415b5944e8175ce16c914c4e3f7700",
    "971b012807069808000000d13a101592ec50cb9920b925f0291915530312ae511035592b6004156d4066ab023449043602523e9700",
    "841b0128111069808000000d13a101c689d280ac9762855cc905c92f8148c8aa981895728881aac95b0020ab6a03355811a24821b01291f4b8",
    "c5019a4195c9a64dba4b140af49b804c0015a6a807262ac9c48b8662320f8664240666711998cc433331990066883330ccd10e980b3334400cd14666199a28cc4980b3334500cd18666199a30cc8980b3334604cd10668a520626688334628050f320c4cd14668c50041e48fc8855e027cc9268183b09dc2de9c78acd668b40e71dbb249386e022a2c412b109882490314f4e19700",
    "47019a4195e5720dc26482749324b14006d8384400a8013411dca1ba01990cc4033119a4066622604d9a400d668e60d030401826c1c8f6e62613789208e850bc5a07c6e9f520a9a0ed3733473966907019a39bcf258380c1bd30166e233493811609b050183b4dc84c059b88c528e0769390980b37118a59c480424843a947ee43346047d44a0dbbd359a486ee05094026c9342476a0306a20ea022004a041509e503fe6e128944601bd8b40f0681136c9a1923811419ca89510ee50662b00209511040262ac668c6aca2640b2671b4b26cc648a6271a2b8",
];

/// The trailer after a PPMd stream ending a file, as every RAR seen writes it.
const TRAILER: [u8; 8] = [0x00, 0xbf, 0x88, 0x67, 0xf6, 0xa9, 0xff, 0xd4];

fn program(kind: u8) -> Vec<u8> {
    let h = PROGRAMS[kind as usize].as_bytes();
    (0..h.len() / 2)
        .map(|i| u8::from_str_radix(std::str::from_utf8(&h[2 * i..2 * i + 2]).unwrap(), 16).unwrap())
        .collect()
}

/// A number of a filter record in its shortest form.
fn vm_number(w: &mut BitWriter, v: u32) {
    if v < 16 {
        w.put(0, 2);
        w.put(v, 4);
    } else if v < 256 {
        w.put(1, 2);
        w.put(v, 8);
    } else if v >= 0xffff_ff00 {
        w.put(1, 2);
        w.put((v >> 4) & 15, 8);
        w.put(v & 15, 4);
    } else if v < 65536 {
        w.put(2, 2);
        w.put(v, 16);
    } else {
        w.put(3, 2);
        w.put(v, 32);
    }
}

/// The record RAR writes for a filter of `kind` with `regs` over `len` bytes from `start`,
/// declared at `p` (positions in the stream), given the programs `progs` and the last one used.
fn filter_raw(progs: &[Prog], last: usize, p: u64, start: u64, len: u32, kind: u8, regs: [u32; 2]) -> Option<Vec<u8>> {
    let idx = progs.iter().position(|q| q.kind == kind).unwrap_or(progs.len());
    let new = idx == progs.len();
    let mut g = 0u8;
    let mut w = BitWriter::new();
    if new || idx != last || progs.is_empty() {
        g |= 0x80;
        vm_number(&mut w, if progs.is_empty() { 0 } else { idx as u32 + 1 });
    }
    let mut s = start.checked_sub(p)?;
    if s >= 258 {
        g |= 0x40;
        s -= 258;
    }
    vm_number(&mut w, u32::try_from(s).ok()?);
    if new || progs[idx].len != len {
        g |= 0x20;
        vm_number(&mut w, len);
    }
    if regs != [0, 0] {
        g |= 0x10;
        let m = (regs[0] != 0) as u32 | ((regs[1] != 0) as u32) << 1;
        w.put(m, 7);
        for r in regs.iter().filter(|&&r| r != 0) {
            vm_number(&mut w, *r);
        }
    }
    if new {
        let code = program(kind);
        vm_number(&mut w, code.len() as u32);
        for b in code {
            w.put(b as u32, 8);
        }
    }
    let body = w.finish();
    let n = body.len();
    let mut raw = Vec::with_capacity(n + 3);
    if n <= 6 {
        raw.push(g | (n as u8 - 1));
    } else if n <= 262 {
        raw.extend([g | 6, (n - 7) as u8]);
    } else {
        raw.extend([g | 7, (n >> 8) as u8, n as u8]);
    }
    raw.extend(body);
    Some(raw)
}

/// Bucket of a length: itself below 8, else 8-11 by its bit length.
#[inline]
fn lb(l: usize) -> usize {
    if l < 8 { l } else { (5 + (usize::BITS - l.leading_zeros()) as usize).min(11) }
}

/// Bucket of a distance (0-7); 8 for none.
#[inline]
fn db(d: usize) -> usize {
    [16, 256, 1024, 4096, 16384, 1 << 16, 1 << 20].iter().filter(|&&x| d >= x).count()
}

/// Whether an explicit or short match of `l` bytes at `d` can be written.
#[inline]
fn writable(l: usize, d: usize) -> bool {
    (l == 2 && d <= 256) || (l >= 3 + bonus(d as u32) as usize && l - bonus(d as u32) as usize <= 258)
}

/// A region of a file: LZ blocks in turn, or a PPMd block (its flags, memory, escape as sent).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Region {
    Lz,
    Ppmd { flags: u8, mem: u8, esc: u8 },
}

/// A file's part as the record has it.
#[derive(Clone, Debug, Default)]
struct Part {
    /// Its regions and the bytes of data of each.
    regions: Vec<(Region, usize)>,
    /// The tokens of each LZ region, without filters; of each PPMd region, its matches as
    /// tokens (`Copy` for the escape 4, `Rep` of index 0 for the escape 5) and literals.
    tokens: Vec<Vec<Token>>,
    /// The filters: where declared (in the file), whether in a PPMd block, the record.
    filters: Vec<(usize, bool, FilterRec)>,
    /// The LZ blocks of each LZ region: their tokens with filters, their tables.
    blocks: Vec<Vec<(usize, Option<Box<Table>>)>>,
    next_header: bool,
    tail: Vec<u8>,
}

/// The contexts of a stream's record.
struct Ctx {
    is_match: Vec<Bit>,
    is_rep: Vec<Bit>,
    rep_idx: Vec<Bit>,
    rep_full: Vec<Bit>,
    rep_shorter: Vec<Bit>,
    pair: Vec<Bit>,
    full: Vec<Bit>,
    shorter: Vec<Bit>,
    escape: Vec<Bit>,
    rank: Vec<Bit>,
    form: Vec<Bit>,
    form_alt: Vec<Bit>,
    run: Vec<Bit>,
    pp_match: Vec<Bit>,
    pp_kind: Vec<Bit>,
    pp_full: Vec<Bit>,
    cut: Vec<Bit>,
    keep: Vec<Bit>,
    predicted: Vec<Bit>,
    lens: Vec<Bit>,
    region: Vec<Bit>,
    filter: Vec<Bit>,
    misc: Vec<Bit>,
    count: Vec<Bit>,
}

/// The lengths of the vectors of [`Ctx`], in the order of its fields.
pub const CTX_LENS: [usize; 24] = [
    2 * 12 * 9 * 3 * 3,
    3 * 12,
    4 * 4,
    12,
    12 * 32,
    3 * 4 * 4 * 2,
    12,
    12 * 32,
    12 * 16,
    12 * 24,
    3 * 2,
    8,
    2,
    2 * 6,
    2,
    2,
    3 + 32,
    4,
    2,
    16 * 2,
    8 + 32 * 2 + 128 * 2,
    8 + 32 * 6 + 2,
    32 * 8,
    32 * 8,
];

impl Ctx {
    fn new() -> Self {
        let v = |i: usize| bits(CTX_LENS[i]);
        Ctx {
            is_match: v(0),
            is_rep: v(1),
            rep_idx: v(2),
            rep_full: v(3),
            rep_shorter: v(4),
            pair: v(5),
            full: v(6),
            shorter: v(7),
            escape: v(8),
            rank: v(9),
            form: v(10),
            form_alt: v(11),
            run: v(12),
            pp_match: v(13),
            pp_kind: v(14),
            pp_full: v(15),
            cut: v(16),
            keep: v(17),
            predicted: v(18),
            lens: v(19),
            region: v(20),
            filter: v(21),
            misc: v(22),
            count: v(23),
        }
    }
}

fn g32(t: &mut [Bit], i: usize) -> &mut [Bit] {
    &mut t[i * 32..i * 32 + 32]
}

/// The bytes of data of LZ tokens, `last` the last match's length before.
fn lz_len(tokens: &[Token], last: &mut u32) -> usize {
    let mut n = 0;
    for t in tokens {
        n += match *t {
            Token::Lit(_) => 1,
            Token::Last => *last as usize,
            Token::Rep { len, .. } | Token::Copy { len, .. } => {
                *last = len;
                len as usize
            }
            Token::Short { .. } => {
                *last = 2;
                2
            }
            Token::Filter(_) => 0,
            Token::Lits(n) => n as usize,
        };
    }
    n
}

/// The streams of a group taken apart into [`Part`]s.
fn take_apart(streams: &[Stream]) -> Result<Vec<Part>> {
    let mut parts = Vec::new();
    let mut last = 0u32;
    let mut esc = 2u8;
    for st in streams {
        let mut p = Part { next_header: st.next_header, tail: st.tail.clone(), ..Part::default() };
        let mut pos = 0usize;
        for b in &st.blocks {
            match b {
                Block::Lz { table, tokens } => {
                    if !matches!(p.regions.last(), Some((Region::Lz, _))) {
                        p.regions.push((Region::Lz, 0));
                        p.tokens.push(Vec::new());
                        p.blocks.push(Vec::new());
                    }
                    let mut at = pos;
                    let mut l = last;
                    for t in tokens {
                        if let Token::Filter(i) = *t {
                            if i as usize != p.filters.len() {
                                return Err(corrupt());
                            }
                            p.filters.push((at, false, st.filters[i as usize].clone()));
                        } else {
                            at += lz_len(std::slice::from_ref(t), &mut l);
                            p.tokens.last_mut().unwrap().push(*t);
                        }
                    }
                    let n = lz_len(tokens, &mut last);
                    pos += n;
                    p.regions.last_mut().unwrap().1 += n;
                    p.blocks.last_mut().unwrap().push((tokens.len(), table.clone()));
                }
                Block::Ppmd { flags, mem, esc: e, syms } => {
                    if flags & 0x40 != 0 {
                        esc = *e;
                    }
                    // the data and the matches of the symbols
                    let mut toks = Vec::new();
                    let mut i = 0;
                    let mut n = 0usize;
                    while i < syms.len() {
                        let s = syms[i];
                        if s != esc {
                            toks.push(Token::Lit(s));
                            n += 1;
                            i += 1;
                            continue;
                        }
                        let c = *syms.get(i + 1).ok_or_else(corrupt)?;
                        match c {
                            0 | 2 => {
                                i += 2;
                                break;
                            }
                            1 => {
                                toks.push(Token::Lit(esc));
                                n += 1;
                                i += 2;
                            }
                            3 => {
                                let f = st.filters.get(p.filters.len()).ok_or_else(corrupt)?;
                                if syms.get(i + 2..i + 2 + f.raw.len()) != Some(&f.raw[..]) {
                                    return Err(corrupt());
                                }
                                p.filters.push((pos + n, true, f.clone()));
                                i += 2 + f.raw.len();
                            }
                            4 => {
                                let b = syms.get(i + 2..i + 6).ok_or_else(corrupt)?;
                                let d = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
                                let l = b[3] as u32 + 32;
                                toks.push(Token::Copy { len: l, dist: d + 2, run: false });
                                n += l as usize;
                                i += 6;
                            }
                            5 => {
                                let l = *syms.get(i + 2).ok_or_else(corrupt)? as u32 + 4;
                                toks.push(Token::Rep { idx: 0, len: l });
                                n += l as usize;
                                i += 3;
                            }
                            _ => return Err(corrupt()),
                        }
                    }
                    if i != syms.len() {
                        return Err(Error("PPMd symbols past the block's end"));
                    }
                    pos += n;
                    p.regions.push((Region::Ppmd { flags: *flags, mem: *mem, esc: *e }, n));
                    p.tokens.push(toks);
                    p.blocks.push(Vec::new());
                }
            }
        }
        parts.push(p);
    }
    Ok(parts)
}

/// The escape of the PPMd block of `region` given the escape before.
fn esc_of(r: Region, esc: u8) -> u8 {
    match r {
        Region::Ppmd { flags, esc: e, .. } if flags & 0x40 != 0 => e,
        _ => esc,
    }
}

/// A filter as the decoder keeps it: where declared (in the file), whether in a PPMd block, where
/// it starts in the file, its length, the filter, r0 and r1, and its record's bytes (the range of
/// the stream's [`Held::raws`]).
#[derive(Clone, Copy, Debug)]
struct Flt {
    decl: usize,
    in_ppmd: bool,
    at: usize,
    len: u32,
    kind: u8,
    regs: [u32; 2],
    raw: (usize, usize),
}

/// A region of a file as the decoder keeps it, with where its tokens and its LZ blocks end in
/// [`Held::toks`] and [`Held::blocks`].
#[derive(Clone, Copy, Debug)]
struct Reg {
    region: Region,
    n: usize,
    toks: usize,
    blocks: usize,
}

/// A file's part as the decoder keeps it: where its regions end in [`Held::regs`] and its tail in
/// [`Held::tails`], and whether the next file starts with a header.
#[derive(Clone, Copy, Debug)]
struct Fil {
    regs: usize,
    tail: usize,
    next_header: bool,
}

/// An LZ block: where its tokens start in [`Held::toks`], their count, and its table (0 none, 1 a
/// table of lengths coded afresh, 2 one of lengths kept from the table before).
#[derive(Clone, Copy, Debug)]
struct Blk {
    start: usize,
    n: usize,
    tab: u8,
}

/// What a stream's record holds, all of it in vectors made once at their final sizes (the
/// counts of its start), filled as the record is read.
struct Held {
    flt: Vec<Flt>,
    raws: Vec<u8>,
    /// The end of each file's filters in `flt`.
    fends: Vec<usize>,
    regs: Vec<Reg>,
    files: Vec<Fil>,
    tails: Vec<u8>,
    /// The tokens of the regions in turn: of an LZ region the tokens with the filters among them,
    /// of a PPMd region its runs of literals, matches and filters.
    toks: Vec<Token>,
    blocks: Vec<Blk>,
}

impl Held {
    fn new(s: &Start) -> Held {
        let files = s.sizes.len();
        Held {
            flt: Vec::with_capacity(s.filters),
            raws: Vec::with_capacity(s.raws),
            fends: Vec::with_capacity(files),
            regs: Vec::with_capacity(s.regions),
            files: Vec::with_capacity(files),
            tails: Vec::with_capacity(s.tails),
            toks: Vec::with_capacity(s.tokens),
            blocks: Vec::with_capacity(s.blocks),
        }
    }

    /// The range of the file `fi`'s filters in `flt`, and of its regions in `regs`.
    fn ranges(&self, fi: usize) -> (Range<usize>, Range<usize>) {
        let before = |v: &[usize], i: usize| if i == 0 { 0 } else { v[i - 1] };
        let regs = (if fi == 0 { 0 } else { self.files[fi - 1].regs })..self.files[fi].regs;
        (before(&self.fends, fi)..self.fends[fi], regs)
    }
}

/// Bytes of what the module holds on WebAssembly (32-bit pointers), checked there by assertions:
/// a stream's data and record, a filter, a region, a file's part, a block, a token, a pair, an
/// adaptive bit, a code.
pub mod wasm {
    pub const STREAM: u64 = 16;
    pub const FLT: u64 = 32;
    pub const REG: u64 = 16;
    pub const FIL: u64 = 12;
    pub const BLK: u64 = 12;
    pub const TOKEN: u64 = 12;
    pub const PAIR: u64 = 8;
    pub const BIT: u64 = 4;
    pub const CODE: u64 = 4;
}

#[cfg(target_family = "wasm")]
const _: () = {
    assert!(size_of::<(&[u8], &[u8])>() as u64 == wasm::STREAM);
    assert!(size_of::<Flt>() as u64 == wasm::FLT);
    assert!(size_of::<Reg>() as u64 == wasm::REG);
    assert!(size_of::<Fil>() as u64 == wasm::FIL);
    assert!(size_of::<Blk>() as u64 == wasm::BLK);
    assert!(size_of::<Token>() as u64 == wasm::TOKEN);
    assert!(size_of::<(usize, usize)>() as u64 == wasm::PAIR);
    assert!(size_of::<Bit>() as u64 == wasm::BIT);
    assert!(size_of::<(u16, u8)>() as u64 == wasm::CODE);
};

/// The first index at or after `from` of `fl`'s filters in a PPMd block (if `ppmd`) or not.
#[inline]
fn next_kind(fl: &[Flt], from: usize, ppmd: bool) -> usize {
    (from..fl.len()).find(|&i| fl[i].in_ppmd == ppmd).unwrap_or(fl.len())
}

/// `t` pushed onto `toks`, which must have room reserved for it.
#[inline]
fn push(toks: &mut Vec<Token>, t: Token) -> Result<()> {
    if toks.len() == toks.capacity() {
        return Err(Error("more tokens than the record counts"));
    }
    toks.push(t);
    Ok(())
}

/// Bytes put onto `v`, which must have room reserved for them.
fn put_bytes(v: &mut Vec<u8>, b: &[u8]) -> Result<()> {
    if v.capacity() - v.len() < b.len() {
        return Err(Error("more bytes than the record counts"));
    }
    v.extend_from_slice(b);
    Ok(())
}

/// The files' filters, coded: each where it is declared and what it does, its record against
/// RAR's for it. The programs' state runs on through the stream. The filters are put onto the
/// end of `h` (room for them reserved); the encoder's `given` are the files' parts.
fn code_filters(bits: &mut impl Bits, ctx: &mut Ctx, sizes: &[usize], given: &[Part], h: &mut Held) -> Result<()> {
    bits.cat(Cat::Head);
    let f = &mut ctx.filter;
    let encoding = !given.is_empty();
    let mut progs: Vec<Prog> = Vec::with_capacity(lz3::MAX_PROGS);
    let mut last_prog = 0usize;
    let mut base = 0u64;
    for (i, &size) in sizes.iter().enumerate() {
        let gf: &[(usize, bool, FilterRec)] = if encoding { &given[i].filters } else { &[] };
        let n = bits.gamma(g32(&mut f[8..], 0), gf.len() as u64)? as usize;
        if n > size + 64 || n > h.flt.capacity() - h.flt.len() {
            return Err(corrupt());
        }
        let (mut end, mut decl) = (0usize, 0usize);
        for k in 0..n {
            let g = gf.get(k).map(|g| (g.0, &g.2));
            let in_ppmd = bits.bit(&mut f[201], gf.get(k).is_some_and(|g| g.1));
            let kind = bits.tree(&mut f[0..8], 3, g.map_or(0, |g| g.1.kind as u32)) as u8;
            if kind as usize >= PROGRAMS.len() {
                return Err(corrupt());
            }
            let gd = g.map_or(0, |g| g.0);
            let ga = g.map_or(0, |g| g.1.at);
            let gap = bits.gamma(g32(&mut f[8..], 1), ga.wrapping_sub(end) as u64)? as usize;
            let at = end.checked_add(gap).filter(|&a| a <= size).ok_or_else(corrupt)?;
            let len = bits.gamma(g32(&mut f[8..], 2), g.map_or(0, |g| g.1.len) as u64)?;
            let len = u32::try_from(len).map_err(|_| corrupt())?;
            end = at.checked_add(len as usize).filter(|&e| e <= size).ok_or_else(corrupt)?;
            let back = bits.gamma(g32(&mut f[8..], 3), (gd.wrapping_sub(decl)) as u64)? as usize;
            let d = decl.checked_add(back).filter(|&d| d <= at).ok_or_else(corrupt)?;
            decl = d;
            let mut regs = [0u32; 2];
            let used = match kind {
                lz3::DELTA | lz3::AUDIO => 1,
                lz3::RGB => 2,
                _ => 0,
            };
            for (r, reg) in regs.iter_mut().enumerate().take(used) {
                *reg = u32::try_from(bits.gamma(g32(&mut f[8..], 4 + r), g.map_or(0, |g| g.1.regs[r]) as u64)?)
                    .map_err(|_| corrupt())?;
            }
            let from = h.raws.len();
            match filter_raw(&progs, last_prog, base + d as u64, base + at as u64, len, kind, regs) {
                Some(p) if bits.bit(&mut f[200], g.is_none_or(|g| g.1.raw == p)) => put_bytes(&mut h.raws, &p)?,
                _ => {
                    let gr = g.map_or(&[][..], |g| &g.1.raw[..]);
                    let n = bits.gamma(g32(&mut f[8..], 0), gr.len() as u64)? as usize;
                    if n == 0 || n > 0x10003 || h.raws.capacity() - from < n {
                        return Err(corrupt());
                    }
                    for i in 0..n {
                        h.raws.push(bits.raws(8, gr.get(i).copied().unwrap_or(0) as u64) as u8);
                    }
                }
            }
            let (start, flen, fkind, fregs) =
                lz3::parse_filter(&h.raws[from..], &mut progs, &mut last_prog, base + d as u64)?;
            if start != base + at as u64 || flen != len || fkind != kind || fregs != regs {
                return Err(Error("a filter record of another meaning"));
            }
            if let Some(g) = g {
                if g.0 != d || gf[k].1 != in_ppmd || g.1.at != at || g.1.raw[..] != h.raws[from..] {
                    return Err(Error("filters coded otherwise"));
                }
            }
            h.flt.push(Flt { decl: d, in_ppmd, at, len, kind, regs, raw: (from, h.raws.len()) });
        }
        h.fends.push(h.flt.len());
        base += size as u64;
        if bits.failed() {
            return Err(corrupt());
        }
    }
    Ok(())
}

/// The contents `d` of a file filtered again by `filters` (the inverse of [`lz3::unfilter`]
/// where it has one), in place. A filter's bytes are copied, no more than `most` of them.
fn refilter(d: &mut [u8], filters: &[Flt], most: usize) -> Result<()> {
    let mut end = 0;
    for f in filters {
        let (at, len) = (f.at, f.len as usize);
        if at < end || at.checked_add(len).is_none_or(|e| e > d.len()) || len > most {
            return Err(Error("a filter out of place"));
        }
        end = at + len;
        let r = &mut d[at..end];
        let src = r.to_vec();
        match f.kind {
            lz3::DELTA => {
                let c = f.regs[0] as usize;
                if !(1..=128).contains(&c) {
                    return Err(corrupt());
                }
                let mut k = 0;
                for ch in 0..c {
                    let mut p = 0u8;
                    for j in (ch..len).step_by(c) {
                        r[k] = p.wrapping_sub(src[j]);
                        p = src[j];
                        k += 1;
                    }
                }
            }
            lz3::E8 | lz3::E8E9 => {
                let mut i = 0;
                while i + 5 <= len {
                    if r[i] == 0xe8 || (f.kind == lz3::E8E9 && r[i] == 0xe9) {
                        let b = i32::from_le_bytes(r[i + 1..i + 5].try_into().unwrap());
                        let c = (at as u32).wrapping_add(i as u32 + 1);
                        // (A − C, for 0 ≤ A < 2^24; A + 2^24 for A < 0 and C ≥ −A; else A)
                        let a = b.wrapping_add(c as i32);
                        let a = if (0..1 << 24).contains(&a) {
                            a
                        } else {
                            let a = b.wrapping_sub(1 << 24);
                            if a < 0 && c >= a.unsigned_abs() { a } else { b }
                        };
                        r[i + 1..i + 5].copy_from_slice(&a.to_le_bytes());
                        i += 5;
                    } else {
                        i += 1;
                    }
                }
            }
            lz3::RGB => {
                let (s, b) = (f.regs[0] as usize, f.regs[1] as usize);
                if len < 3 || s > len || s < 3 || b > 2 {
                    return Err(corrupt());
                }
                // undo the green, then the prediction (on the copy, which is not needed after)
                let mut o = src;
                let mut i = b;
                while i + 2 < len {
                    let g = o[i + 1];
                    o[i] = o[i].wrapping_sub(g);
                    o[i + 2] = o[i + 2].wrapping_sub(g);
                    i += 3;
                }
                let mut k = 0;
                for c in 0..3 {
                    let mut p = 0u8;
                    for j in (c..len).step_by(3) {
                        if j >= s {
                            let up = o[j - s + 3] as i32;
                            let ul = o[j - s] as i32;
                            let pv = p as i32;
                            let d1 = (up - ul).abs();
                            let d2 = (pv - ul).abs();
                            let d3 = (up - ul + pv - ul).abs();
                            if d1 > d2 || d1 > d3 {
                                p = if d2 <= d3 { up as u8 } else { ul as u8 };
                            }
                        }
                        r[k] = p.wrapping_sub(o[j]);
                        p = o[j];
                        k += 1;
                    }
                }
            }
            lz3::AUDIO => {
                let c = f.regs[0] as usize;
                if !(1..=128).contains(&c) {
                    return Err(corrupt());
                }
                let mut k = 0;
                for ch in 0..c {
                    let mut a = lz3::Audio::default();
                    for j in (ch..len).step_by(c) {
                        r[k] = a.encode(src[j]);
                        k += 1;
                    }
                }
            }
            _ => return Err(corrupt()),
        }
    }
    Ok(())
}

/// The bytes where the unfiltered data `u` differs from the contents filtered again `v`, coded
/// and applied to `v`.
fn code_fixes(bits: &mut impl Bits, ctx: &mut Ctx, u: Option<&[u8]>, v: &mut [u8]) -> Result<()> {
    bits.cat(Cat::Head);
    let diff: Vec<usize> = match u {
        Some(u) => (0..u.len()).filter(|&i| u[i] != v[i]).collect(),
        None => Vec::new(),
    };
    let n = bits.gamma(g32(&mut ctx.misc, 2), diff.len() as u64)? as usize;
    if n > v.len() {
        return Err(corrupt());
    }
    let mut at = 0usize;
    for k in 0..n {
        let (g, x) = match u {
            Some(u) => (diff[k] - at, u[diff[k]] ^ v[diff[k]]),
            None => (0, 0),
        };
        let gap = bits.gamma(g32(&mut ctx.misc, 3), g as u64)? as usize;
        at = at.checked_add(gap).filter(|&a| a < v.len()).ok_or_else(corrupt)?;
        v[at] ^= bits.raws(8, x as u64) as u8;
        at += 1;
    }
    Ok(())
}

/// The escape RAR picks for a PPMd block of data `d`: the least frequent byte of its first 64
/// KiB, the highest of those.
fn predict_esc(d: &[u8]) -> u8 {
    let mut c = [0u32; 256];
    d[..d.len().min(1 << 16)].iter().for_each(|&b| c[b as usize] += 1);
    (0..256).rev().min_by_key(|&b| c[b]).unwrap() as u8
}

/// The files' regions, coded: their kinds, lengths and PPMd blocks' parameters; how each file
/// ends (whether the next starts with a header; the bytes after the end). They are put onto the
/// end of `h` (room for them reserved); PPMd blocks use no more than `model` MiB.
fn code_regions(
    bits: &mut impl Bits,
    ctx: &mut Ctx,
    u: &[u8],
    sizes: &[usize],
    given: &[Part],
    h: &mut Held,
    model: u32,
) -> Result<()> {
    bits.cat(Cat::Blocks);
    let r = &mut ctx.region;
    let encoding = !given.is_empty();
    let mut fs = 0usize;
    for (i, &size) in sizes.iter().enumerate() {
        let gp = given.get(i);
        let gr: &[(Region, usize)] = gp.map_or(&[], |p| &p.regions);
        let from = h.regs.len();
        let mut pos = 0usize;
        let mut prev_ppmd = false;
        loop {
            let made = h.regs.len() - from;
            let g = gr.get(made).copied();
            // (LZ and PPMd regions alternate, but for PPMd blocks in turn)
            let ppmd = if prev_ppmd || made == 0 {
                bits.bit(&mut r[prev_ppmd as usize], matches!(g, Some((Region::Ppmd { .. }, _))))
            } else {
                true
            };
            let (region, gn) = if ppmd {
                let (gf, gm, ge) = match g {
                    Some((Region::Ppmd { flags, mem, esc }, _)) => (flags, mem, esc),
                    _ => (0, 0, 0),
                };
                let flags = bits.raws(7, gf as u64) as u8;
                let mem = if flags & 0x20 != 0 { bits.raws(8, gm as u64) as u8 } else { 0 };
                if flags & 0x20 != 0 && mem as u32 + 1 > model {
                    return Err(Error("a PPMd model larger than the module's"));
                }
                (Region::Ppmd { flags, mem, esc: ge }, g.map_or(0, |g| g.1))
            } else {
                (Region::Lz, g.map_or(0, |g| g.1))
            };
            // its length: to the file's end, or as it is
            let rest = size - pos;
            let n = if bits.bit(&mut r[2 + ppmd as usize], gn == rest) {
                rest
            } else {
                let n = bits.gamma(&mut r[8 + 32 * ppmd as usize..40 + 32 * ppmd as usize], gn as u64)? as usize;
                if n > rest {
                    return Err(corrupt());
                }
                n
            };
            let region = match region {
                Region::Ppmd { flags, mem, esc: ge } if flags & 0x40 != 0 => {
                    let pred = predict_esc(&u[fs + pos..fs + pos + n]);
                    let e = if bits.bit(&mut r[4], ge == pred) { pred } else { bits.raws(8, ge as u64) as u8 };
                    Region::Ppmd { flags, mem, esc: e }
                }
                r => r,
            };
            if h.regs.len() == h.regs.capacity() {
                return Err(Error("more regions than the record counts"));
            }
            h.regs.push(Reg { region, n, toks: 0, blocks: 0 });
            pos += n;
            prev_ppmd = ppmd;
            if pos == size && !bits.bit(&mut r[5 + ppmd as usize], gr.len() > made + 1) {
                break;
            }
            if made > size + 64 || bits.failed() {
                return Err(corrupt());
            }
        }
        if encoding {
            let same = h.regs[from..].iter().map(|r| (r.region, r.n)).eq(gr.iter().copied());
            if !same {
                return Err(Error("regions coded otherwise"));
            }
        }
        let ends_ppmd = matches!(h.regs.last(), Some(Reg { region: Region::Ppmd { .. }, .. }));
        // how the file ends
        let gnext = gp.is_some_and(|p| p.next_header);
        let next_header = if !ends_ppmd {
            bits.bit(&mut r[7], gnext)
        } else if !gnext && encoding {
            return Err(corrupt());
        } else {
            true
        };
        let expect: &[u8] = if ends_ppmd { &TRAILER } else { &[] };
        let gtail: &[u8] = gp.map_or(&[], |p| &p.tail);
        if !bits.bit(&mut r[72 + ends_ppmd as usize], gtail == expect) {
            let n = bits.gamma(&mut r[74..106], gtail.len() as u64)? as usize;
            if n > 1 << 20 || h.tails.capacity() - h.tails.len() < n {
                return Err(corrupt());
            }
            for k in 0..n {
                h.tails.push(bits.raws(8, gtail.get(k).copied().unwrap_or(0) as u64) as u8);
            }
        } else {
            put_bytes(&mut h.tails, expect)?;
        }
        h.files.push(Fil { regs: h.regs.len(), tail: h.tails.len(), next_header });
        fs += size;
    }
    Ok(())
}

/// The form of an LZ match: explicit, a repeated distance (its index), short, the last again.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Form {
    Copy,
    Rep(u8),
    Short,
    Last,
}

impl Form {
    fn number(self) -> u32 {
        match self {
            Form::Copy => 0,
            Form::Rep(i) => 1 + i as u32,
            Form::Short => 5,
            Form::Last => 6,
        }
    }
    fn of_number(n: u32) -> Option<Form> {
        match n {
            0 => Some(Form::Copy),
            1..=4 => Some(Form::Rep(n as u8 - 1)),
            5 => Some(Form::Short),
            6 => Some(Form::Last),
            _ => None,
        }
    }
}

/// The walk over the unfiltered data `u` of the files' regions, coding their tokens (the
/// encoder's `given` are the files' parts). The tokens are put onto the end of `h.toks` (room
/// for them reserved), the filters among them where they are declared; where each region's end.
fn walk(
    u: &[u8],
    sizes: &[usize],
    reach: usize,
    bits: &mut impl Bits,
    ctx: &mut Ctx,
    given: &[Part],
    h: &mut Held,
) -> Result<()> {
    let encoding = !given.is_empty();
    let Held { flt, fends, regs, files, toks, .. } = h;
    let mut mf = Mf::new(u, reach);
    let (mut pairs, mut next_pairs) = (Vec::with_capacity(Mf::PAIRS), Vec::with_capacity(Mf::PAIRS));
    let mut rep = [0usize; 4];
    let mut last = (0usize, 0usize);
    let mut fs = 0usize;
    for (fi, &size) in sizes.iter().enumerate() {
        let mut pos = fs;
        let (r0, r1) = (if fi == 0 { 0 } else { files[fi - 1].regs }, files[fi].regs);
        let fl = &flt[if fi == 0 { 0 } else { fends[fi - 1] }..fends[fi]];
        // (the next filter in an LZ block, and in a PPMd block)
        let (mut lzc, mut ppc) = (next_kind(fl, 0, false), next_kind(fl, 0, true));
        for gi in r0..r1 {
            let (region, n) = (regs[gi].region, regs[gi].n);
            let re = pos + n;
            let given: &[Token] = if encoding { &given[fi].tokens[gi - r0] } else { &[] };
            let mut k = 0usize;
            if let Region::Ppmd { .. } = region {
                // where the matches are: runs of a byte (the escape 5), others (the escape 4)
                bits.cat(Cat::Start);
                let mut prev = 0usize;
                let mut run = 0u32;
                while pos < re {
                    // (the filters declared here; one declared within a match stays)
                    while ppc < fl.len() && fl[ppc].decl == pos - fs {
                        if run > 0 {
                            push(toks, Token::Lits(run))?;
                            run = 0;
                        }
                        push(toks, Token::Filter(ppc as u32))?;
                        ppc = next_kind(fl, ppc + 1, true);
                    }
                    mf.advance(pos);
                    let g = if encoding { *given.get(k).ok_or_else(corrupt)? } else { Token::Lit(0) };
                    k += 1;
                    let rl = if pos > 0 { lcp_up(u, pos - 1, pos, 259.min(re - pos)) } else { 0 };
                    let rc = if rl < 4 { 0 } else { 1 + lb(rl).min(11) / 3 };
                    if !bits.bit(&mut ctx.pp_match[prev * 6 + rc.min(5)], !matches!(g, Token::Lit(_))) {
                        run += 1;
                        pos += 1;
                        prev = 0;
                        continue;
                    }
                    let five = bits.bit(&mut ctx.pp_kind[(rl >= 4) as usize], matches!(g, Token::Rep { .. }));
                    let t = if five {
                        let top = rl.min(259);
                        let l = match g {
                            Token::Rep { len, .. } => len as usize,
                            _ => 0,
                        };
                        let l = if bits.bit(&mut ctx.pp_full[0], l == top) {
                            top
                        } else {
                            bits.gamma(g32(&mut ctx.misc, 4), l.wrapping_sub(4) as u64)? as usize + 4
                        };
                        if !(4..=259).contains(&l) || l > rl {
                            return Err(corrupt());
                        }
                        Token::Rep { idx: 0, len: l as u32 }
                    } else {
                        let (gl, gd) = match g {
                            Token::Copy { len, dist, .. } => (len as usize, dist as usize),
                            _ => (0, 0),
                        };
                        let d = bits.gamma(g32(&mut ctx.misc, 5), gd.wrapping_sub(2) as u64)? as usize + 2;
                        let l = bits.gamma(g32(&mut ctx.misc, 6), gl.wrapping_sub(32) as u64)? as usize + 32;
                        if l > 287 || d > (1 << 24) + 1 || d > pos || pos + l > re || lcp_up(u, pos - d, pos, l) < l {
                            return Err(corrupt());
                        }
                        Token::Copy { len: l as u32, dist: d as u32, run: false }
                    };
                    if encoding && t != g {
                        return Err(Error("a PPMd match coded otherwise"));
                    }
                    pos += match t {
                        Token::Copy { len, .. } | Token::Rep { len, .. } => len as usize,
                        _ => unreachable!(),
                    };
                    if run > 0 {
                        push(toks, Token::Lits(run))?;
                        run = 0;
                    }
                    push(toks, t)?;
                    prev = 1;
                }
                // (the filters declared at the end, or within a match)
                while ppc < fl.len() && fl[ppc].decl <= pos - fs {
                    if run > 0 {
                        push(toks, Token::Lits(run))?;
                        run = 0;
                    }
                    push(toks, Token::Filter(ppc as u32))?;
                    ppc = next_kind(fl, ppc + 1, true);
                }
                if run > 0 {
                    push(toks, Token::Lits(run))?;
                }
            } else {
                let mut prev = 0usize;
                let mut ahead = usize::MAX;
                while pos < re {
                    // (the filters declared here; one declared within a match is past)
                    while lzc < fl.len() {
                        let d = fl[lzc].decl;
                        if d > pos - fs {
                            break;
                        }
                        if d < pos - fs {
                            return Err(Error("a filter declared within a match"));
                        }
                        push(toks, Token::Filter(lzc as u32))?;
                        lzc = next_kind(fl, lzc + 1, false);
                    }
                    mf.advance(pos);
                    let cap = (re - pos).min(CAP);
                    let ok = |c: [usize; 4]| move |l: usize, d: usize| writable(l, d) && !c.contains(&d);
                    if ahead == pos {
                        std::mem::swap(&mut pairs, &mut next_pairs);
                    } else {
                        mf.pairs(pos, cap, ok(rep), &mut pairs);
                    }
                    ahead = usize::MAX;
                    let mut rl = [0usize; 4];
                    for i in 0..4 {
                        let d = rep[i];
                        if d != 0 && d <= pos && d <= reach {
                            rl[i] = lcp_up(u, pos - d, pos, cap);
                        }
                    }
                    let rbest = *rl.iter().max().unwrap();
                    let rarg = rl.iter().position(|&l| l == rbest).unwrap();
                    let longest = pairs.last().copied().unwrap_or((0, 0));
                    let cur = longest.0.max(rbest);
                    let lazy = if cur >= 2 && pos + 1 < re {
                        mf.advance(pos + 1);
                        mf.pairs(pos + 1, (re - pos - 1).min(CAP), ok(rep), &mut next_pairs);
                        ahead = pos + 1;
                        let nx = next_pairs.last().map_or(0, |p| p.0);
                        (nx > cur) as usize + (nx > cur + 1) as usize
                    } else {
                        0
                    };
                    let repc = if rbest < 2 {
                        0
                    } else if rbest < longest.0 {
                        1
                    } else {
                        2
                    };
                    let dc = if longest.0 == 0 { 8 } else { db(longest.1) };
                    let c = (((prev * 12 + lb(longest.0)) * 9 + dc) * 3 + lazy) * 3 + repc;
                    let g = if encoding { *given.get(k).ok_or_else(corrupt)? } else { Token::Lit(0) };
                    k += 1;
                    bits.cat(Cat::Start);
                    if !bits.bit(&mut ctx.is_match[c], !matches!(g, Token::Lit(_))) {
                        if encoding && g != Token::Lit(u[pos]) {
                            return Err(Error("a literal not of the data"));
                        }
                        push(toks, Token::Lit(u[pos]))?;
                        pos += 1;
                        prev = 0;
                        continue;
                    }
                    let (g_len, g_dist, g_form, g_run) = match g {
                        Token::Copy { len, dist, run } => (len as usize, dist as usize, Form::Copy, run),
                        Token::Rep { idx, len } => (len as usize, rep[idx as usize & 3], Form::Rep(idx), false),
                        Token::Short { dist } => (2, dist as usize, Form::Short, false),
                        Token::Last => (last.1, last.0, Form::Last, false),
                        _ => (0, 0, Form::Copy, false),
                    };
                    bits.cat(Cat::Pair);
                    let is_rep =
                        rbest >= 2 && bits.bit(&mut ctx.is_rep[repc * 12 + lb(longest.0)], rep.contains(&g_dist));
                    let (len, dist) = if is_rep {
                        let gi = rep.iter().position(|&d| d == g_dist).unwrap_or(0) as u32;
                        let i = bits.tree(&mut ctx.rep_idx[rarg * 4..rarg * 4 + 4], 2, gi) as usize;
                        let m = rl[i];
                        if m < 2 {
                            return Err(corrupt());
                        }
                        bits.cat(Cat::Len);
                        let len = if bits.bit(&mut ctx.rep_full[lb(m)], g_len == m.min(LONG)) {
                            m.min(LONG)
                        } else {
                            let top = m.min(CAP);
                            let s = bits.gamma(
                                &mut ctx.rep_shorter[lb(m) * 32..lb(m) * 32 + 32],
                                top.wrapping_sub(g_len) as u64,
                            )?;
                            top.checked_sub(s as usize).ok_or_else(corrupt)?
                        };
                        (len, rep[i])
                    } else {
                        let m = pairs.len();
                        let gj = pairs.iter().rposition(|&(l, d)| d == g_dist && l >= g_len);
                        let gj = gj.map_or(m, |i| m - 1 - i);
                        let mut j = 0;
                        while j < m {
                            let i = m - 1 - j;
                            let (dl, dd) = match i {
                                0 => (3, 3),
                                _ => {
                                    let bl = |d: usize| (usize::BITS - d.leading_zeros()) as usize;
                                    (
                                        (pairs[i].0 - pairs[i - 1].0 - 1).min(2),
                                        (bl(pairs[i].1) - bl(pairs[i - 1].1)).min(3),
                                    )
                                }
                            };
                            if !bits.bit(&mut ctx.pair[((j.min(2) * 4 + dl) * 4 + dd) * 2 + (i == 0) as usize], j < gj)
                            {
                                break;
                            }
                            j += 1;
                        }
                        if j < m {
                            let (top, dist) = pairs[m - 1 - j];
                            bits.cat(Cat::Len);
                            let len = if bits.bit(&mut ctx.full[lb(top)], g_len == top.min(LONG)) {
                                top.min(LONG)
                            } else {
                                let s = bits.gamma(
                                    &mut ctx.shorter[lb(top) * 32..lb(top) * 32 + 32],
                                    top.wrapping_sub(g_len) as u64,
                                )?;
                                top.checked_sub(s as usize).ok_or_else(corrupt)?
                            };
                            (len, dist)
                        } else {
                            bits.cat(Cat::Escape);
                            let lc = lb(longest.0);
                            let len =
                                bits.gamma(&mut ctx.escape[lc * 16..lc * 16 + 16], g_len.wrapping_sub(2) as u64)?;
                            let len = (len as usize).checked_add(2).filter(|&l| l <= cap).ok_or_else(corrupt)?;
                            let c2 = rep;
                            let okd = move |d: usize| writable(len, d) && !c2.contains(&d);
                            let rank = if encoding { mf.rank(pos, len, g_dist, okd) } else { 0 };
                            let lc = lb(len);
                            let r = bits.gamma(&mut ctx.rank[lc * 24..lc * 24 + 24], rank as u64)? as usize;
                            let dist = match mf.nth(pos, len, r, okd) {
                                Some(d) => d,
                                None => (bits.gamma(g32(&mut ctx.misc, 0), g_dist.wrapping_sub(1) as u64)? as usize)
                                    .wrapping_add(1),
                            };
                            (len, dist)
                        }
                    };
                    if dist == 0
                        || dist > pos
                        || dist > reach
                        || len < 2
                        || len > cap
                        || lcp_up(u, pos - dist, pos, len) < len
                    {
                        return Err(Error("a match that does not fit"));
                    }
                    // how it is written: RAR's way or another
                    bits.cat(Cat::Form);
                    let pred = if (dist, len) == last {
                        Form::Last
                    } else if let Some(i) = rep.iter().position(|&d| d == dist) {
                        Form::Rep(i as u8)
                    } else if len == 2 {
                        Form::Short
                    } else {
                        Form::Copy
                    };
                    let form = if pred == Form::Copy || pred == Form::Short {
                        pred
                    } else {
                        let pk = matches!(pred, Form::Rep(_)) as usize;
                        if bits.bit(&mut ctx.form[pk], g_form == pred) {
                            pred
                        } else {
                            Form::of_number(bits.tree(&mut ctx.form_alt, 3, g_form.number())).ok_or_else(corrupt)?
                        }
                    };
                    let t = match form {
                        Form::Copy => {
                            if len < 3 + bonus(dist as u32) as usize || len - bonus(dist as u32) as usize > 258 {
                                return Err(corrupt());
                            }
                            // (a run of the low distance bits, for distances of their slots)
                            let run = dist > 32 && bits.bit(&mut ctx.run[0], g_run);
                            rep.copy_within(0..3, 1);
                            rep[0] = dist;
                            Token::Copy { len: len as u32, dist: dist as u32, run }
                        }
                        Form::Short => {
                            if len != 2 || dist > 256 {
                                return Err(corrupt());
                            }
                            rep.copy_within(0..3, 1);
                            rep[0] = dist;
                            Token::Short { dist: dist as u32 }
                        }
                        Form::Rep(i) => {
                            let i = i as usize;
                            if rep[i] != dist || len > 257 {
                                return Err(corrupt());
                            }
                            rep.copy_within(0..i, 1);
                            rep[0] = dist;
                            Token::Rep { idx: i as u8, len: len as u32 }
                        }
                        Form::Last => {
                            if (dist, len) != last {
                                return Err(corrupt());
                            }
                            Token::Last
                        }
                    };
                    last = (dist, len);
                    if encoding && t != g {
                        return Err(Error("a match coded otherwise"));
                    }
                    push(toks, t)?;
                    pos += len;
                    prev = 1;
                    if bits.failed() {
                        return Err(corrupt());
                    }
                }
                // (the filters declared at the end)
                while lzc < fl.len() && fl[lzc].decl == re - fs {
                    push(toks, Token::Filter(lzc as u32))?;
                    lzc = next_kind(fl, lzc + 1, false);
                }
            }
            if encoding && k != given.len() {
                return Err(Error("tokens past the data"));
            }
            regs[gi].toks = toks.len();
            pos = re;
        }
        if lzc < fl.len() {
            return Err(Error("a filter declared out of the LZ blocks"));
        }
        if ppc < fl.len() {
            return Err(Error("a filter declared out of the PPMd blocks"));
        }
        fs += size;
    }
    Ok(())
}

/// The LZ blocks of the regions, coded: their sizes against RAR's buffer of symbols, whether
/// they keep the lengths before. Every LZ block has a table but a file's first where the file
/// before says it goes on with its codes. The blocks are put onto the end of `h.blocks` (room for
/// them reserved), where each region's end.
fn code_blocks(bits: &mut impl Bits, ctx: &mut Ctx, given: &[Part], h: &mut Held) -> Result<()> {
    bits.cat(Cat::Blocks);
    let encoding = !given.is_empty();
    let Held { regs, files, blocks, .. } = h;
    let mut header = true;
    let mut after_ppmd = false;
    for fi in 0..files.len() {
        let (r0, r1) = (if fi == 0 { 0 } else { files[fi - 1].regs }, files[fi].regs);
        for gi in r0..r1 {
            let t0 = if gi == 0 { 0 } else { regs[gi - 1].toks };
            let tn = regs[gi].toks - t0;
            if !matches!(regs[gi].region, Region::Lz) {
                after_ppmd = true;
                regs[gi].blocks = blocks.len();
                continue;
            }
            let gb: &[(usize, Option<Box<Table>>)] = if encoding { &given[fi].blocks[gi - r0] } else { &[] };
            let from = blocks.len();
            let mut at = 0;
            loop {
                let made = blocks.len() - from;
                let first_of_file = gi == r0 && made == 0;
                let k = (tn - at).min(SYMBOLS - first_of_file as usize);
                let g = gb.get(made);
                let gn = g.map_or(0, |g| g.0);
                let n = if bits.bit(&mut ctx.cut[first_of_file as usize], gn == k) {
                    k
                } else {
                    bits.gamma(&mut ctx.cut[3..35], gn as u64)? as usize
                };
                if at + n > tn || blocks.len() == blocks.capacity() {
                    return Err(corrupt());
                }
                let tab = if first_of_file && !header {
                    0
                } else {
                    let kc = (made == 0) as usize * 2 + after_ppmd as usize;
                    let gk = g.is_none_or(|g| g.1.as_ref().is_some_and(|t| t.keep));
                    1 + bits.bit(&mut ctx.keep[kc], gk) as u8
                };
                blocks.push(Blk { start: t0 + at, n, tab });
                after_ppmd = false;
                at += n;
                if at == tn && !bits.bit(&mut ctx.cut[2], gb.len() > made + 1) {
                    break;
                }
                if made > tn + 64 || bits.failed() {
                    return Err(corrupt());
                }
            }
            if encoding {
                let same = blocks.len() - from == gb.len()
                    && blocks[from..].iter().zip(gb).all(|(a, b)| {
                        a.n == b.0
                            && (a.tab == 0) == b.1.is_none()
                            && (a.tab == 2) == b.1.as_ref().is_some_and(|t| t.keep)
                    });
                if !same {
                    return Err(Error("blocks coded otherwise"));
                }
            }
            regs[gi].blocks = blocks.len();
        }
        header = files[fi].next_header;
    }
    Ok(())
}

/// The table of the LZ block `k` of `blocks` (of `tab` 1 or 2), coded: the lengths of Huffman's
/// construction over the symbols of its tokens and those of the blocks without tables after it
/// (the 256 ending each counted), or as they are (`given`), and the code lengths against the
/// lengths `prev` of the table before.
fn code_table(
    bits: &mut impl Bits,
    ctx: &mut Ctx,
    blocks: &[Blk],
    toks: &[Token],
    k: usize,
    prev: &[u8; lz3::LENS],
    given: Option<&Table>,
) -> Result<Table> {
    let keep = blocks[k].tab == 2;
    let bases = lz3::bases();
    let mut c = [vec![0u32; lz3::MAIN], vec![0; lz3::DIST], vec![0; lz3::LOW], vec![0; lz3::LEN]];
    let mut low = (0u32, 0u32);
    let mut j = k;
    loop {
        let b = blocks[j];
        for tk in toks.get(b.start..b.start + b.n).ok_or_else(corrupt)? {
            lz3::symbols(tk, &bases, &mut low, |a, s| c[a][s] += 1);
        }
        c[0][256] += 1;
        j += 1;
        if j == blocks.len() || blocks[j].tab != 0 {
            break;
        }
    }
    let mut lens = Vec::with_capacity(lz3::LENS);
    for f in &c {
        lens.extend(huff::lengths(f, huff::MAX));
    }
    let lens = code_lens(bits, ctx, 0, lens, given.map(|g| &g.lens[..]))?;
    let lens: [u8; lz3::LENS] = lens.try_into().unwrap();
    let prev = if keep { *prev } else { [0; lz3::LENS] };
    let mut pc = vec![0u32; lz3::PRE];
    lz3::rle(&lens, &prev).iter().for_each(|r| pc[r.0 as usize] += 1);
    let pre = huff::lengths(&pc, huff::MAX);
    let pre = code_lens(bits, ctx, 1, pre, given.map(|g| &g.pre[..]))?;
    if bits.failed() {
        return Err(corrupt());
    }
    Ok(Table { keep, pre: pre.try_into().unwrap(), lens })
}

/// Code lengths predicted `pred`, coded as they are (`given`): all as predicted, or each.
fn code_lens(bits: &mut impl Bits, ctx: &mut Ctx, c: usize, pred: Vec<u8>, given: Option<&[u8]>) -> Result<Vec<u8>> {
    if bits.bit(&mut ctx.predicted[c], given.is_none_or(|g| g == &pred[..])) {
        return Ok(pred);
    }
    let mut out = pred;
    for (i, l) in out.iter_mut().enumerate() {
        let g = given.map_or(0, |g| g[i]);
        if !bits.bit(&mut ctx.lens[c * 16 + *l as usize], g == *l) {
            *l = bits.raws(4, g as u64) as u8;
        }
    }
    Ok(out)
}

/// Bytes the tables' work takes at most (the counts, the lengths, Huffman's heap, the run-length
/// coding): with room for the vectors that grow, and for the holes that leaves. The decoder takes
/// them from the stack once before the blocks and gives them back, so that what it takes does not
/// depend on the data.
pub const SCRATCH: usize = 64 << 10;

/// The stream written onto the end of `out` (room for it reserved): each file's blocks in turn,
/// the tables of the LZ blocks read from the record (`bits`, `ctx`) as they are reached, which is
/// the record's order, and the PPMd blocks' symbols made of the data `u` and the tokens. The end
/// of each file's part in `out` is put in `ends` if given; the encoder gives its parts `given`.
#[allow(clippy::too_many_arguments)]
fn write_stream(
    bits: &mut impl Bits,
    ctx: &mut Ctx,
    enc: &mut Encoder3,
    u: &[u8],
    sizes: &[usize],
    h: &Held,
    given: &[Part],
    out: &mut Vec<u8>,
    mut ends: Option<&mut Vec<usize>>,
) -> Result<()> {
    let encoding = !given.is_empty();
    if !encoding {
        std::hint::black_box(Vec::<u8>::with_capacity(SCRATCH));
    }
    let (mut fs, mut esc, mut header) = (0usize, 2u8, true);
    for (fi, &size) in sizes.iter().enumerate() {
        let (franges, rranges) = h.ranges(fi);
        let fl = &h.flt[franges];
        let raw = |i: u32| fl.get(i as usize).map(|f| &h.raws[f.raw.0..f.raw.1]);
        let mut w = BitWriter::default();
        w.out = std::mem::take(out);
        let mut pos = 0usize;
        let mut first = true;
        for gi in rranges.clone() {
            let r = h.regs[gi];
            let (t0, b0) = if gi == 0 { (0, 0) } else { (h.regs[gi - 1].toks, h.regs[gi - 1].blocks) };
            match r.region {
                Region::Lz => {
                    for bj in b0..r.blocks {
                        let b = h.blocks[bj];
                        let table = if b.tab == 0 {
                            None
                        } else {
                            let gt = if encoding {
                                given[fi].blocks[gi - rranges.start][bj - b0].1.as_deref()
                            } else {
                                None
                            };
                            Some(code_table(bits, ctx, &h.blocks, &h.toks, bj, enc.prev(), gt)?)
                        };
                        let end = (gi + 1 == rranges.end && bj + 1 == r.blocks).then_some(h.files[fi].next_header);
                        let tokens = h.toks.get(b.start..b.start + b.n).ok_or_else(corrupt)?;
                        enc.lz_block(&mut w, table.as_ref(), first && !header, tokens, &raw, end)?;
                        first = false;
                    }
                }
                Region::Ppmd { flags, mem, esc: e } => {
                    if first && !header {
                        return Err(Error("a PPMd block without a header"));
                    }
                    esc = esc_of(r.region, esc);
                    let last = gi + 1 == rranges.end;
                    let data = u.get(fs + pos..fs + pos + r.n).ok_or_else(corrupt)?;
                    enc.ppmd_block(&mut w, flags, mem, e, |put| {
                        let mut at = 0usize;
                        for t in &h.toks[t0..r.toks] {
                            match *t {
                                Token::Lits(k) => {
                                    for &b in data.get(at..at + k as usize).ok_or_else(corrupt)? {
                                        if b == esc {
                                            put(esc);
                                            put(1);
                                        } else {
                                            put(b);
                                        }
                                    }
                                    at += k as usize;
                                }
                                Token::Filter(i) => {
                                    put(esc);
                                    put(3);
                                    for &b in raw(i).ok_or_else(corrupt)? {
                                        put(b);
                                    }
                                }
                                Token::Copy { len, dist, .. } => {
                                    let d = dist - 2;
                                    for s in [esc, 4, (d >> 16) as u8, (d >> 8) as u8, d as u8, (len - 32) as u8] {
                                        put(s);
                                    }
                                    at += len as usize;
                                }
                                Token::Rep { len, .. } => {
                                    for s in [esc, 5, (len - 4) as u8] {
                                        put(s);
                                    }
                                    at += len as usize;
                                }
                                _ => return Err(corrupt()),
                            }
                        }
                        put(esc);
                        put(if last { 2 } else { 0 });
                        Ok(())
                    })?;
                    first = false;
                }
            }
            pos += r.n;
        }
        *out = w.finish();
        let t0 = if fi == 0 { 0 } else { h.files[fi - 1].tail };
        out.extend_from_slice(&h.tails[t0..h.files[fi].tail]);
        header = h.files[fi].next_header;
        if let Some(e) = ends.as_mut() {
            e.push(out.len());
        }
        fs += size;
    }
    Ok(())
}

pub use crate::recons5::Split;

/// Splits the stream of the files of `parts` (their packed data in turn, of `sizes` bytes each)
/// with the window `dict`. Fails where it does not decode, or is not what encoding its parse
/// gives.
pub fn split(packed: &[&[u8]], sizes: &[usize], dict: u64) -> Result<Split> {
    split_with(packed, sizes, dict, true)
}

/// The count of the elements the walk puts in [`Held::toks`] for `parts`: an LZ region's
/// tokens and filters, a PPMd region's runs of literals, matches and filters.
fn elements(parts: &[Part]) -> usize {
    let mut k = 0;
    for p in parts {
        k += p.filters.iter().filter(|f| !f.1).count();
        // (the filters in PPMd blocks, put where declared, the first one blocking the others)
        let pp: Vec<usize> = p.filters.iter().filter(|f| f.1).map(|f| f.0).collect();
        let (mut pc, mut pos) = (0, 0);
        for (ri, &(region, n)) in p.regions.iter().enumerate() {
            if let Region::Lz = region {
                k += p.tokens[ri].len();
                pos += n;
                continue;
            }
            let mut run = false;
            for t in &p.tokens[ri] {
                while pc < pp.len() && pp[pc] == pos {
                    k += 1 + run as usize;
                    run = false;
                    pc += 1;
                }
                match *t {
                    Token::Copy { len, .. } | Token::Rep { len, .. } => {
                        k += 1 + run as usize;
                        run = false;
                        pos += len as usize;
                    }
                    _ => {
                        run = true;
                        pos += 1;
                    }
                }
            }
            while pc < pp.len() && pp[pc] <= pos {
                k += 1 + run as usize;
                run = false;
                pc += 1;
            }
            k += run as usize;
        }
    }
    k
}

/// [`split`], failing at once at a PPMd block unless `ppmd`.
pub fn split_with(packed: &[&[u8]], sizes: &[usize], dict: u64, ppmd: bool) -> Result<Split> {
    if packed.len() != sizes.len() || packed.is_empty() {
        return Err(corrupt());
    }
    let mut dec = Decoder3::new(dict);
    dec.ppmd_on = ppmd;
    let mut streams = Vec::new();
    let mut data = Vec::new();
    for (&p, &size) in packed.iter().zip(sizes) {
        let at = dec.out.len();
        let st = dec.file(p, size)?;
        let mut file = dec.out[at..].to_vec();
        lz3::unfilter(&mut file, &st.filters)?;
        data.extend_from_slice(&file);
        streams.push(st);
    }
    let u = dec.out;
    let reach = streams
        .iter()
        .flat_map(|s| &s.blocks)
        .flat_map(|b| match b {
            Block::Lz { tokens, .. } => tokens.as_slice(),
            _ => &[],
        })
        .map(|t| match t {
            Token::Copy { dist, .. } | Token::Short { dist } => *dist as usize,
            _ => 0,
        })
        .max()
        .unwrap_or(0);
    let model = streams
        .iter()
        .flat_map(|s| &s.blocks)
        .map(|b| match b {
            Block::Ppmd { flags, mem, .. } if flags & 0x20 != 0 => *mem as u32 + 1,
            _ => 0,
        })
        .max()
        .unwrap_or(0);
    let mut e = Enc::new(Cat::Head as usize);
    let mut ctx = Ctx::new();
    e.gamma(g32(&mut ctx.misc, 1), sizes.len() as u64 - 1)?;
    for &s in sizes {
        e.gamma(g32(&mut ctx.misc, 1), s as u64)?;
    }
    e.gamma(g32(&mut ctx.misc, 1), reach as u64)?;
    let parts = take_apart(&streams)?;
    let sum = |f: &dyn Fn(&Part) -> usize| parts.iter().map(f).sum::<usize>();
    let counts = Counts {
        filters: sum(&|p| p.filters.len()),
        raws: sum(&|p| p.filters.iter().map(|f| f.2.raw.len()).sum()),
        regions: sum(&|p| p.regions.len()),
        tails: sum(&|p| p.tail.len()),
        blocks: sum(&|p| p.blocks.iter().map(Vec::len).sum()),
        tokens: elements(&parts),
        delta: parts.iter().flat_map(|p| &p.filters).map(|f| f.2.len as usize).max().unwrap_or(0),
        model,
    };
    counts.code(&mut e, &mut ctx)?;
    let mut h = Held {
        flt: Vec::with_capacity(counts.filters),
        raws: Vec::with_capacity(counts.raws),
        fends: Vec::new(),
        regs: Vec::with_capacity(counts.regions),
        files: Vec::new(),
        tails: Vec::with_capacity(counts.tails),
        toks: Vec::with_capacity(counts.tokens),
        blocks: Vec::with_capacity(counts.blocks),
    };
    code_filters(&mut e, &mut ctx, sizes, &parts, &mut h)?;
    let mut v = data.clone();
    refile(&mut v, sizes, &h, counts.delta)?;
    code_fixes(&mut e, &mut ctx, Some(&u), &mut v)?;
    code_regions(&mut e, &mut ctx, &u, sizes, &parts, &mut h, u32::MAX)?;
    walk(&u, sizes, reach, &mut e, &mut ctx, &parts, &mut h)?;
    if h.toks.len() != counts.tokens {
        return Err(Error("a count of tokens that does not fit"));
    }
    code_blocks(&mut e, &mut ctx, &parts, &mut h)?;
    // (the stream written again from what the record has)
    let mut enc = Encoder3::with_model(model)?;
    let (mut out, mut ends) = (Vec::new(), Vec::new());
    write_stream(&mut e, &mut ctx, &mut enc, &u, sizes, &h, &parts, &mut out, Some(&mut ends))?;
    let mut at = 0;
    for (&end, &p) in ends.iter().zip(packed) {
        if out[at..end] != *p {
            return Err(Error("a stream encoded otherwise"));
        }
        at = end;
    }
    let mut record = vec![3u8];
    record.extend(e.c.finish());
    start_of(&record, data.len())?;
    Ok(Split { data, record, cost: e.cost, ppmd: model })
}

/// The files' contents `data` of `sizes` filtered again by the filters of `h`, in place; the
/// filters copy no more than `most` bytes.
fn refile(data: &mut [u8], sizes: &[usize], h: &Held, most: usize) -> Result<()> {
    let (mut at, mut from) = (0, 0);
    for (i, &s) in sizes.iter().enumerate() {
        refilter(&mut data[at..at + s], &h.flt[from..h.fends[i]], most)?;
        (at, from) = (at + s, h.fends[i]);
    }
    Ok(())
}

/// The counts a record starts with, after the sizes of the files and the reach: what a decoder
/// reserves, and the record must come to exactly: filters, the bytes of their records, regions,
/// the bytes after the files' parts, blocks, elements of the walk (`Held::toks`), the most bytes
/// of a filter (that the filters copy), and the MiB of the largest PPMd model.
struct Counts {
    filters: usize,
    raws: usize,
    regions: usize,
    tails: usize,
    blocks: usize,
    tokens: usize,
    delta: usize,
    model: u32,
}

impl Counts {
    fn code(&self, bits: &mut impl Bits, ctx: &mut Ctx) -> Result<()> {
        let c = &mut ctx.count;
        let all = [self.filters, self.raws, self.regions, self.tails, self.blocks, self.tokens, self.delta];
        for (i, v) in all.into_iter().chain([self.model as usize]).enumerate() {
            bits.gamma(g32(c, i), v as u64)?;
        }
        Ok(())
    }
}

/// What a record starts with: the sizes of the stream's files, its reach, and the counts.
pub struct Start {
    pub sizes: Vec<usize>,
    pub reach: usize,
    pub filters: usize,
    pub raws: usize,
    pub regions: usize,
    pub tails: usize,
    pub blocks: usize,
    pub tokens: usize,
    pub delta: usize,
    pub model: u32,
}

/// The start of `record` for a stream of `n` bytes of data, with the decoder and the contexts as
/// they are after it.
fn start<'a>(record: &'a [u8], n: usize, ctx: &mut Ctx) -> Result<(Start, Dec<'a>)> {
    let (&tag, rest) = record.split_first().ok_or_else(corrupt)?;
    if tag != 3 {
        return Err(corrupt());
    }
    let mut dec = Dec(ArithDecoder::new(rest));
    let files = dec.gamma(g32(&mut ctx.misc, 1), 0)? as usize + 1;
    if files > n + 1 {
        return Err(corrupt());
    }
    let mut sizes = Vec::with_capacity(files);
    let mut total = 0usize;
    for _ in 0..files {
        let s = dec.gamma(g32(&mut ctx.misc, 1), 0)? as usize;
        total = total.checked_add(s).filter(|&t| t <= n).ok_or_else(corrupt)?;
        sizes.push(s);
    }
    if total != n {
        return Err(corrupt());
    }
    let reach = dec.gamma(g32(&mut ctx.misc, 1), 0)? as usize;
    // (what a record can hold: filters fewer than the bytes and 64 a file, a record's bytes
    // as RAR writes it or as the record gives them, regions and blocks as the checks of
    // `code_regions` and `code_blocks` bound them, a token a byte at least)
    let mut i = 0;
    let mut count = |most: usize| -> Result<usize> {
        let v = dec.gamma(g32(&mut ctx.count, i), 0)? as usize;
        i += 1;
        if v > most {
            return Err(corrupt());
        }
        Ok(v)
    };
    let filters = count(n + 64 * files)?;
    let raws = count(300 * filters + record.len())?;
    let regions = count(n + 65 * files)?;
    let tails = count(8 * files + record.len())?;
    // (the blocks are bounded by the tokens, counted after them)
    let blocks = count(usize::MAX)?;
    let tokens = count(n + filters)?;
    let delta = count(n)?;
    let model = count(256)? as u32;
    if blocks > tokens + 64 * regions {
        return Err(corrupt());
    }
    Ok((Start { sizes, reach, filters, raws, regions, tails, blocks, tokens, delta, model }, dec))
}

/// The start of `record` for a stream of `n` bytes of data: what the memory plan follows
/// ([`crate::plan`]).
pub fn start_of(record: &[u8], n: usize) -> Result<Start> {
    Ok(start(record, n, &mut Ctx::new())?.0)
}

/// The stream that `data` and `record` rebuild, written onto `out` (room for it reserved), its
/// PPMd models of at most `ppmd_limit` MiB.
pub fn join_onto(data: &[u8], record: &[u8], out: &mut Vec<u8>, ppmd_limit: u32) -> Result<()> {
    let mut ctx = Ctx::new();
    let (st, mut dec) = start(record, data.len(), &mut ctx)?;
    if st.model > ppmd_limit {
        return Err(Error("a PPMd model larger than the module's"));
    }
    let sizes = &st.sizes[..];
    let mut u = data.to_vec();
    let mut h = Held::new(&st);
    code_filters(&mut dec, &mut ctx, sizes, &[], &mut h)?;
    if h.flt.len() != st.filters || h.raws.len() != st.raws {
        return Err(Error("a count of filters that does not fit"));
    }
    refile(&mut u, sizes, &h, st.delta)?;
    code_fixes(&mut dec, &mut ctx, None, &mut u)?;
    code_regions(&mut dec, &mut ctx, &u, sizes, &[], &mut h, st.model)?;
    if h.regs.len() != st.regions || h.tails.len() != st.tails {
        return Err(Error("a count of regions that does not fit"));
    }
    walk(&u, sizes, st.reach, &mut dec, &mut ctx, &[], &mut h)?;
    if h.toks.len() != st.tokens {
        return Err(Error("a count of tokens that does not fit"));
    }
    code_blocks(&mut dec, &mut ctx, &[], &mut h)?;
    if h.blocks.len() != st.blocks {
        return Err(Error("a count of blocks that does not fit"));
    }
    let mut enc = Encoder3::with_model(st.model)?;
    write_stream(&mut dec, &mut ctx, &mut enc, &u, sizes, &h, &[], out, None)
}

/// [`join_onto`] onto a new vector.
pub fn join(data: &[u8], record: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    join_onto(data, record, &mut out, u32::MAX)?;
    Ok(out)
}
