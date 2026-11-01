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

use aqr_common::arith::{ArithDecoder, Bit};
use aqr_common::msb::BitWriter;

use crate::huff;
use crate::lz3::{self, Block, Decoder3, Encoder3, FilterRec, Prog, Stream, Table, Token, bonus};
use crate::rec::{Bits, Cat, Enc, Mf, bits, lcp};
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
}

impl Ctx {
    fn new() -> Self {
        Ctx {
            is_match: bits(2 * 12 * 9 * 3 * 3),
            is_rep: bits(3 * 12),
            rep_idx: bits(4 * 4),
            rep_full: bits(12),
            rep_shorter: bits(12 * 32),
            pair: bits(3 * 4 * 4 * 2),
            full: bits(12),
            shorter: bits(12 * 32),
            escape: bits(12 * 16),
            rank: bits(12 * 24),
            form: bits(3 * 2),
            form_alt: bits(8),
            run: bits(2),
            pp_match: bits(2 * 6),
            pp_kind: bits(2),
            pp_full: bits(2),
            cut: bits(3 + 32),
            keep: bits(4),
            predicted: bits(2),
            lens: bits(16 * 2),
            region: bits(8 + 32 * 2 + 128 * 2),
            filter: bits(8 + 32 * 6 + 2),
            misc: bits(32 * 8),
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

/// Each file's LZ regions' tokens with the filters put back where they are declared
/// (`Token::Filter(i)` the file's filter `i`), by region (None for PPMd regions).
fn merge_filters(parts: &[Part]) -> Result<Vec<Vec<Option<Vec<Token>>>>> {
    let mut out = Vec::new();
    let mut last = 0u32;
    for p in parts {
        let mut fs = p.filters.iter().enumerate().filter(|f| !f.1.1).map(|(i, f)| (i, f.0)).peekable();
        let mut pos = 0usize;
        let nr = p.regions.len();
        let mut regions = Vec::with_capacity(nr);
        for (ri, &(r, n)) in p.regions.iter().enumerate() {
            if !matches!(r, Region::Lz) {
                regions.push(None);
                pos += n;
                continue;
            }
            let end = pos + n;
            let mut v = Vec::with_capacity(p.tokens[ri].len());
            let mut at = pos;
            for t in &p.tokens[ri] {
                while let Some(&(i, d)) = fs.peek() {
                    if d > at {
                        break;
                    }
                    if d < at {
                        return Err(Error("a filter declared within a match"));
                    }
                    v.push(Token::Filter(i as u32));
                    fs.next();
                }
                v.push(*t);
                at += lz_len(std::slice::from_ref(t), &mut last);
            }
            // (at the region's end, unless an LZ region follows: none does)
            while let Some(&(i, d)) = fs.peek() {
                if d != end {
                    break;
                }
                v.push(Token::Filter(i as u32));
                fs.next();
            }
            if at != end {
                return Err(corrupt());
            }
            regions.push(Some(v));
            pos = end;
        }
        if fs.next().is_some() {
            return Err(Error("a filter declared out of the LZ blocks"));
        }
        out.push(regions);
    }
    Ok(out)
}

/// The streams the parts make: each LZ region's blocks with their filters, each PPMd region's
/// symbols.
fn put_together(parts: &[Part]) -> Result<Vec<Stream>> {
    let lz = merge_filters(parts)?;
    let mut out = Vec::new();
    let mut esc = 2u8;
    for (p, lz) in parts.iter().zip(lz) {
        let mut st = Stream {
            next_header: p.next_header,
            tail: p.tail.clone(),
            filters: p.filters.iter().map(|f| f.2.clone()).collect(),
            ..Stream::default()
        };
        let nr = p.regions.len();
        let mut pf = p.filters.iter().filter(|f| f.1).peekable();
        let mut pos = 0usize;
        for (ri, (&(r, n), v)) in p.regions.iter().zip(lz).enumerate() {
            match r {
                Region::Lz => {
                    let v = v.ok_or_else(corrupt)?;
                    let mut k = 0;
                    for (bn, table) in &p.blocks[ri] {
                        let tokens = v.get(k..k + bn).ok_or_else(corrupt)?.to_vec();
                        k += bn;
                        st.blocks.push(Block::Lz { table: table.clone(), tokens });
                    }
                    if k != v.len() {
                        return Err(Error("tokens past the blocks"));
                    }
                }
                Region::Ppmd { flags, mem, esc: e } => {
                    esc = esc_of(r, esc);
                    let mut syms = Vec::with_capacity(n + 2);
                    let mut at = pos;
                    let mut put = |syms: &mut Vec<u8>, at: usize, end: bool| {
                        while let Some(f) = pf.next_if(|f| f.0 == at || (end && f.0 <= at)) {
                            syms.extend([esc, 3]);
                            syms.extend_from_slice(&f.2.raw);
                        }
                    };
                    for t in &p.tokens[ri] {
                        put(&mut syms, at, false);
                        at += match *t {
                            Token::Copy { len, .. } | Token::Rep { len, .. } => len as usize,
                            _ => 1,
                        };
                        match *t {
                            Token::Lit(b) if b == esc => syms.extend([esc, 1]),
                            Token::Lit(b) => syms.push(b),
                            Token::Copy { len, dist, .. } => {
                                let d = dist - 2;
                                syms.extend([esc, 4, (d >> 16) as u8, (d >> 8) as u8, d as u8, (len - 32) as u8])
                            }
                            Token::Rep { len, .. } => syms.extend([esc, 5, (len - 4) as u8]),
                            _ => return Err(corrupt()),
                        }
                    }
                    put(&mut syms, at, true);
                    syms.extend([esc, if ri + 1 == nr { 2 } else { 0 }]);
                    st.blocks.push(Block::Ppmd { flags, mem, esc: e, syms });
                }
            }
            pos += n;
        }
        if pf.next().is_some() {
            return Err(Error("a filter declared out of the PPMd blocks"));
        }
        out.push(st);
    }
    Ok(out)
}

/// The files' filters, coded: each where it is declared and what it does, its record against
/// RAR's for it. The programs' state runs on through the stream.
fn code_filters(
    bits: &mut impl Bits,
    ctx: &mut Ctx,
    sizes: &[usize],
    parts: &mut [Part],
    encoding: bool,
) -> Result<()> {
    bits.cat(Cat::Head);
    let f = &mut ctx.filter;
    let mut progs: Vec<Prog> = Vec::new();
    let mut last_prog = 0usize;
    let mut base = 0u64;
    for (i, &size) in sizes.iter().enumerate() {
        let given = if encoding { std::mem::take(&mut parts[i].filters) } else { Vec::new() };
        let n = bits.gamma(g32(&mut f[8..], 0), given.len() as u64)? as usize;
        if n > size + 64 {
            return Err(corrupt());
        }
        let mut out = Vec::with_capacity(n);
        let (mut end, mut decl) = (0usize, 0usize);
        for k in 0..n {
            let g = given.get(k).map(|g| (g.0, &g.2));
            let in_ppmd = bits.bit(&mut f[201], given.get(k).is_some_and(|g| g.1));
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
                *reg = bits.gamma(g32(&mut f[8..], 4 + r), g.map_or(0, |g| g.1.regs[r]) as u64)? as u32;
            }
            let pred = filter_raw(&progs, last_prog, base + d as u64, base + at as u64, len, kind, regs);
            let raw = match pred {
                Some(p) if bits.bit(&mut f[200], g.is_none_or(|g| g.1.raw == p)) => p,
                _ => {
                    let gr = g.map_or(&[][..], |g| &g.1.raw[..]);
                    let n = bits.gamma(g32(&mut f[8..], 0), gr.len() as u64)? as usize;
                    if n == 0 || n > 0x10003 {
                        return Err(corrupt());
                    }
                    (0..n).map(|i| bits.raws(8, gr.get(i).copied().unwrap_or(0) as u64) as u8).collect()
                }
            };
            let (start, flen, fkind, fregs) = lz3::parse_filter(&raw, &mut progs, &mut last_prog, base + d as u64)?;
            if start != base + at as u64 || flen != len || fkind != kind || fregs != regs {
                return Err(Error("a filter record of another meaning"));
            }
            out.push((d, in_ppmd, FilterRec { raw, at, len, kind, regs }));
        }
        if encoding && out != given {
            return Err(Error("filters coded otherwise"));
        }
        parts[i].filters = out;
        base += size as u64;
        if bits.failed() {
            return Err(corrupt());
        }
    }
    Ok(())
}

/// The contents `d` of a file filtered again by `filters` (the inverse of [`lz3::unfilter`]
/// where it has one), in place.
fn refilter(d: &mut [u8], filters: &[(usize, bool, FilterRec)]) -> Result<()> {
    let mut end = 0;
    for (_, _, f) in filters {
        let (at, len) = (f.at, f.len as usize);
        if at < end || at.checked_add(len).is_none_or(|e| e > d.len()) {
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
                // undo the green, then the prediction
                let mut o = src.clone();
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
/// ends (whether the next starts with a header; the bytes after the end).
fn code_regions(
    bits: &mut impl Bits,
    ctx: &mut Ctx,
    u: &[u8],
    sizes: &[usize],
    parts: &mut [Part],
    encoding: bool,
) -> Result<()> {
    bits.cat(Cat::Blocks);
    let r = &mut ctx.region;
    let mut fs = 0usize;
    let mut esc = 2u8;
    for (i, &size) in sizes.iter().enumerate() {
        let given = if encoding { std::mem::take(&mut parts[i].regions) } else { Vec::new() };
        let mut out = Vec::new();
        let mut pos = 0usize;
        let mut prev_ppmd = false;
        loop {
            let g = given.get(out.len()).copied();
            // (LZ and PPMd regions alternate, but for PPMd blocks in turn)
            let ppmd = if prev_ppmd || out.is_empty() {
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
            esc = esc_of(region, esc);
            out.push((region, n));
            pos += n;
            prev_ppmd = ppmd;
            if pos == size && !bits.bit(&mut r[5 + ppmd as usize], given.len() > out.len()) {
                break;
            }
            if out.len() > size + 64 || bits.failed() {
                return Err(corrupt());
            }
        }
        if encoding && out != given {
            return Err(Error("regions coded otherwise"));
        }
        let ends_ppmd = matches!(out.last(), Some((Region::Ppmd { .. }, _)));
        parts[i].regions = out;
        // how the file ends
        let p = &mut parts[i];
        if !ends_ppmd {
            p.next_header = bits.bit(&mut r[7], p.next_header);
        } else if !p.next_header && encoding {
            return Err(corrupt());
        } else {
            p.next_header = true;
        }
        let expect: &[u8] = if ends_ppmd { &TRAILER } else { &[] };
        if !bits.bit(&mut r[72 + ends_ppmd as usize], p.tail == expect) {
            let n = bits.gamma(&mut r[74..106], p.tail.len() as u64)? as usize;
            if n > 1 << 20 {
                return Err(corrupt());
            }
            let t: Vec<u8> = (0..n).map(|k| bits.raws(8, p.tail.get(k).copied().unwrap_or(0) as u64) as u8).collect();
            p.tail = t;
        } else {
            p.tail = expect.to_vec();
        }
        fs += size;
        let _ = esc;
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

/// The walk over the unfiltered data `u` of the files' regions, coding their tokens.
fn walk(
    u: &[u8],
    sizes: &[usize],
    reach: usize,
    bits: &mut impl Bits,
    ctx: &mut Ctx,
    parts: &mut [Part],
    encoding: bool,
) -> Result<()> {
    let mut mf = Mf::new(u, reach);
    let (mut pairs, mut next_pairs) = (Vec::new(), Vec::new());
    let mut rep = [0usize; 4];
    let mut last = (0usize, 0usize);
    let mut fs = 0usize;
    for (fi, &size) in sizes.iter().enumerate() {
        let mut pos = fs;
        let nr = parts[fi].regions.len();
        for ri in 0..nr {
            let (region, n) = parts[fi].regions[ri];
            let re = pos + n;
            let given = if encoding { std::mem::take(&mut parts[fi].tokens[ri]) } else { Vec::new() };
            let mut made = Vec::new();
            let mut k = 0usize;
            if let Region::Ppmd { .. } = region {
                // where the matches are: runs of a byte (the escape 5), others (the escape 4)
                bits.cat(Cat::Start);
                let mut prev = 0usize;
                while pos < re {
                    mf.advance(pos);
                    let g = if encoding { *given.get(k).ok_or_else(corrupt)? } else { Token::Lit(0) };
                    k += 1;
                    let run = if pos > 0 { lcp(u, pos - 1, pos, 259.min(re - pos)) } else { 0 };
                    let rc = if run < 4 { 0 } else { 1 + lb(run).min(11) / 3 };
                    if !bits.bit(&mut ctx.pp_match[prev * 6 + rc.min(5)], !matches!(g, Token::Lit(_))) {
                        made.push(Token::Lit(u[pos]));
                        pos += 1;
                        prev = 0;
                        continue;
                    }
                    let five = bits.bit(&mut ctx.pp_kind[(run >= 4) as usize], matches!(g, Token::Rep { .. }));
                    let t = if five {
                        let top = run.min(259);
                        let l = match g {
                            Token::Rep { len, .. } => len as usize,
                            _ => 0,
                        };
                        let l = if bits.bit(&mut ctx.pp_full[0], l == top) {
                            top
                        } else {
                            bits.gamma(g32(&mut ctx.misc, 4), l.wrapping_sub(4) as u64)? as usize + 4
                        };
                        if !(4..=259).contains(&l) || l > run {
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
                        if l > 287 || d > (1 << 24) + 1 || d > pos || pos + l > re || lcp(u, pos - d, pos, l) < l {
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
                    made.push(t);
                    prev = 1;
                }
            } else {
                let mut prev = 0usize;
                let mut ahead = usize::MAX;
                while pos < re {
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
                            rl[i] = lcp(u, pos - d, pos, cap);
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
                        made.push(Token::Lit(u[pos]));
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
                        || lcp(u, pos - dist, pos, len) < len
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
                    made.push(t);
                    pos += len;
                    prev = 1;
                    if bits.failed() {
                        return Err(corrupt());
                    }
                }
            }
            if encoding && k != given.len() {
                return Err(Error("tokens past the data"));
            }
            parts[fi].tokens[ri] = made;
            pos = re;
        }
        fs += size;
    }
    Ok(())
}

/// The LZ blocks of the regions, coded: their sizes against RAR's buffer of symbols, whether
/// they keep the lengths before, their tables against Huffman's lengths. Every LZ block has a
/// table but a file's first where the file before says it goes on with its codes.
fn code_blocks(bits: &mut impl Bits, ctx: &mut Ctx, parts: &mut [Part], encoding: bool) -> Result<()> {
    bits.cat(Cat::Blocks);
    let bases = lz3::bases();
    let lz = merge_filters(parts)?;
    // the blocks' sizes and keep bits; all the blocks in turn
    let mut flat: Vec<(usize, usize, usize)> = Vec::new();
    let mut header = true;
    let mut after_ppmd = false;
    for (fi, p) in parts.iter_mut().enumerate() {
        for (ri, t) in lz[fi].iter().enumerate() {
            let Some(t) = t else {
                after_ppmd = true;
                continue;
            };
            let given = if encoding { std::mem::take(&mut p.blocks[ri]) } else { Vec::new() };
            let mut made: Vec<(usize, Option<Box<Table>>)> = Vec::new();
            let mut at = 0;
            loop {
                let first_of_file = ri == 0 && made.is_empty();
                let k = (t.len() - at).min(SYMBOLS - first_of_file as usize);
                let g = given.get(made.len());
                let gn = g.map_or(0, |g| g.0);
                let n = if bits.bit(&mut ctx.cut[first_of_file as usize], gn == k) {
                    k
                } else {
                    bits.gamma(&mut ctx.cut[3..35], gn as u64)? as usize
                };
                if at + n > t.len() {
                    return Err(corrupt());
                }
                let table = if first_of_file && !header {
                    None
                } else {
                    let kc = (made.is_empty() as usize) * 2 + after_ppmd as usize;
                    let gk = g.is_none_or(|g| g.1.as_ref().is_some_and(|t| t.keep));
                    let keep = bits.bit(&mut ctx.keep[kc], gk);
                    Some(Box::new(Table { keep, pre: [0; lz3::PRE], lens: [0; lz3::LENS] }))
                };
                flat.push((fi, ri, made.len()));
                made.push((n, table));
                after_ppmd = false;
                at += n;
                if at == t.len() && !bits.bit(&mut ctx.cut[2], given.len() > made.len()) {
                    break;
                }
                if made.len() > t.len() + 64 || bits.failed() {
                    return Err(corrupt());
                }
            }
            if encoding {
                let same = made.len() == given.len()
                    && made
                        .iter()
                        .zip(&given)
                        .all(|(a, b)| a.0 == b.0 && a.1.as_ref().map(|t| t.keep) == b.1.as_ref().map(|t| t.keep));
                if !same {
                    return Err(Error("blocks coded otherwise"));
                }
                // (the tables as they are, coded below)
                made = given;
            }
            p.blocks[ri] = made;
        }
        header = p.next_header || matches!(p.regions.last(), Some((Region::Ppmd { .. }, _)));
    }
    // the tables, each over the symbols of its block and the blocks without tables after it,
    // the 256 ending each block counted
    let mut prev = [0u8; lz3::LENS];
    let mut k = 0;
    while k < flat.len() {
        let (fi, ri, bi) = flat[k];
        if parts[fi].blocks[ri][bi].1.is_none() {
            k += 1;
            continue;
        }
        let mut c = [vec![0u32; lz3::MAIN], vec![0; lz3::DIST], vec![0; lz3::LOW], vec![0; lz3::LEN]];
        let mut low = (0u32, 0u32);
        let mut j = k;
        loop {
            let (f, r, b) = flat[j];
            let at: usize = parts[f].blocks[r][..b].iter().map(|x| x.0).sum();
            let n = parts[f].blocks[r][b].0;
            let t = lz[f][r].as_ref().ok_or_else(corrupt)?;
            for tk in &t[at..at + n] {
                lz3::symbols(tk, &bases, &mut low, |a, s| c[a][s] += 1);
            }
            c[0][256] += 1;
            j += 1;
            if j == flat.len() || parts[flat[j].0].blocks[flat[j].1][flat[j].2].1.is_some() {
                break;
            }
        }
        let mut lens = Vec::with_capacity(lz3::LENS);
        for f in &c {
            lens.extend(huff::lengths(f, huff::MAX));
        }
        let tb = parts[fi].blocks[ri][bi].1.as_mut().ok_or_else(corrupt)?;
        let given = encoding.then_some((tb.lens, tb.pre));
        let lens = code_lens(bits, ctx, 0, lens, given.as_ref().map(|g| &g.0[..]))?;
        let lens: [u8; lz3::LENS] = lens.try_into().unwrap();
        if !tb.keep {
            prev = [0; lz3::LENS];
        }
        let mut pc = vec![0u32; lz3::PRE];
        lz3::rle(&lens, &prev).iter().for_each(|r| pc[r.0 as usize] += 1);
        let pre = huff::lengths(&pc, huff::MAX);
        let pre = code_lens(bits, ctx, 1, pre, given.as_ref().map(|g| &g.1[..]))?;
        tb.lens = lens;
        tb.pre = pre.try_into().unwrap();
        prev = lens;
        if bits.failed() {
            return Err(corrupt());
        }
        k = j;
    }
    Ok(())
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

pub use crate::recons5::Split;

/// Splits the stream of the files of `parts` (their packed data in turn, of `sizes` bytes each)
/// with the window `dict`. Fails where it does not decode, or is not what encoding its parse
/// gives.
pub fn split(packed: &[&[u8]], sizes: &[usize], dict: u64) -> Result<Split> {
    if packed.len() != sizes.len() || packed.is_empty() {
        return Err(corrupt());
    }
    let mut dec = Decoder3::new(dict);
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
    let mut e = Enc::new();
    let mut ctx = Ctx::new();
    e.gamma(g32(&mut ctx.misc, 1), sizes.len() as u64 - 1)?;
    for &s in sizes {
        e.gamma(g32(&mut ctx.misc, 1), s as u64)?;
    }
    e.gamma(g32(&mut ctx.misc, 1), reach as u64)?;
    let mut parts = take_apart(&streams)?;
    code_filters(&mut e, &mut ctx, sizes, &mut parts, true)?;
    let mut v = data.clone();
    refile(&mut v, sizes, &parts)?;
    code_fixes(&mut e, &mut ctx, Some(&u), &mut v)?;
    code_regions(&mut e, &mut ctx, &u, sizes, &mut parts, true)?;
    walk(&u, sizes, reach, &mut e, &mut ctx, &mut parts, true)?;
    code_blocks(&mut e, &mut ctx, &mut parts, true)?;
    let made = put_together(&parts)?;
    if made != streams {
        return Err(Error("streams put together otherwise"));
    }
    let mut enc = Encoder3::new();
    let mut out = Vec::new();
    let mut header = true;
    for (st, &p) in made.iter().zip(packed) {
        let at = out.len();
        enc.file(st, header, &mut out)?;
        header = st.next_header;
        if out[at..] != *p {
            return Err(Error("a stream encoded otherwise"));
        }
    }
    let mut record = vec![3u8];
    record.extend(e.c.finish());
    Ok(Split { data, record, cost: e.cost })
}

/// The files' contents `data` of `sizes` filtered again by the filters of `parts`, in place.
fn refile(data: &mut [u8], sizes: &[usize], parts: &[Part]) -> Result<()> {
    let mut at = 0;
    for (i, &s) in sizes.iter().enumerate() {
        refilter(&mut data[at..at + s], &parts[i].filters)?;
        at += s;
    }
    Ok(())
}

/// The stream that `data` and `record` rebuild, written onto `out`; the sizes of its files'
/// parts.
pub fn join_onto(data: &[u8], record: &[u8], out: &mut Vec<u8>) -> Result<Vec<usize>> {
    let (&tag, rest) = record.split_first().ok_or_else(corrupt)?;
    if tag != 3 {
        return Err(corrupt());
    }
    let mut dec = ArithDecoder::new(rest);
    let mut ctx = Ctx::new();
    let n = dec.gamma(g32(&mut ctx.misc, 1), 0)? as usize + 1;
    if n > data.len() + 1 {
        return Err(corrupt());
    }
    let mut sizes = Vec::with_capacity(n);
    let mut total = 0usize;
    for _ in 0..n {
        let s = dec.gamma(g32(&mut ctx.misc, 1), 0)? as usize;
        total = total.checked_add(s).filter(|&t| t <= data.len()).ok_or_else(corrupt)?;
        sizes.push(s);
    }
    if total != data.len() {
        return Err(corrupt());
    }
    let reach = dec.gamma(g32(&mut ctx.misc, 1), 0)? as usize;
    let mut parts = vec![Part::default(); n];
    code_filters(&mut dec, &mut ctx, &sizes, &mut parts, false)?;
    let mut u = data.to_vec();
    refile(&mut u, &sizes, &parts)?;
    code_fixes(&mut dec, &mut ctx, None, &mut u)?;
    code_regions(&mut dec, &mut ctx, &u, &sizes, &mut parts, false)?;
    for p in &mut parts {
        p.tokens = vec![Vec::new(); p.regions.len()];
        p.blocks = vec![Vec::new(); p.regions.len()];
    }
    walk(&u, &sizes, reach, &mut dec, &mut ctx, &mut parts, false)?;
    code_blocks(&mut dec, &mut ctx, &mut parts, false)?;
    let streams = put_together(&parts)?;
    let mut enc = Encoder3::new();
    let mut sizes = Vec::with_capacity(n);
    let mut header = true;
    for st in &streams {
        let at = out.len();
        enc.file(st, header, out)?;
        header = st.next_header;
        sizes.push(out.len() - at);
    }
    Ok(sizes)
}

/// [`join_onto`] onto a new vector.
pub fn join(data: &[u8], record: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    join_onto(data, record, &mut out)?;
    Ok(out)
}

/// Bytes of the working memory of a stream of `n` bytes of data, besides its data and record
/// and the PPMd models.
pub fn memory(n: u64) -> u64 {
    n + Mf::memory(n) + 4 * std::mem::size_of::<Token>() as u64 * n / 2 + n + (1 << 20)
}
