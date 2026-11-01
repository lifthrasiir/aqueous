//! The record of a RAR5 stream given its data (`FORMAT.md`, "The RAR5 record"): a solid
//! group's files (or a file alone), their filters, the parse, the blocks and their tables.
//!
//! The data is the files' contents. Their unfiltered data, which the parse copies from, is the
//! contents filtered again (the filters known from the record) and corrected where that is not
//! what the stream decoded. Whether a match starts is coded under contexts of the matches the
//! data offers; which match, as one of the four last distances or as one of the longest matches
//! hash chains find, nearest first; how it is written (an explicit copy, a repeated distance,
//! the last copy again) against RAR's choice. Blocks are cut where RAR's buffer of 16384 units
//! fills, their tables the lengths of Huffman's construction over the tokens they serve.

use aqr_common::arith::{ArithDecoder, Bit};

use crate::huff;
use crate::lz5::{self, Block, Decoder5, Encoder5, Filter, Placed, Stream, Tables, Token, bonus};
use crate::rec::{Bits, CATS, Cat, Enc, Mf, bits, lcp};
use crate::{Error, Result, corrupt};

/// The longest copy RAR writes.
const LONG: usize = 4096;
/// The longest copy of the format: 4097, with the bonus of the farthest distances.
const CAP: usize = 4100;
/// Units of a block (a token one, a filter more), RAR's buffer.
const UNITS: usize = 16384;
/// The span of the parser's segments, at whose starts repeated distances are not known.
const SEGMENT: usize = 1 << 17;

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

/// The units of a token in RAR's buffer.
#[inline]
fn units(t: &Token) -> usize {
    match t {
        Token::Filter(f) => 6 + f.start_bytes as usize,
        _ => 1,
    }
}

/// The fewest bytes for a filter's field `v`.
#[inline]
fn min_bytes(v: u32) -> u8 {
    (1 + (v > 0xff) as u8 + (v > 0xffff) as u8 + (v > 0xff_ffff) as u8).max(1)
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
    cut: Vec<Bit>,
    tables: Vec<Bit>,
    predicted: Vec<Bit>,
    lens: Vec<Bit>,
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
            form: bits(2 * 2),
            form_alt: bits(8),
            cut: bits(2 + 32 + 1),
            tables: bits(4),
            predicted: bits(2),
            lens: bits(16),
            filter: bits(5 * 4 + 32 * 6 + 8),
            misc: bits(32 * 8),
        }
    }
}

/// A slice of 32 adaptive bits of `t` for gammas, the `i`th.
fn g32(t: &mut [Bit], i: usize) -> &mut [Bit] {
    &mut t[i * 32..i * 32 + 32]
}

/// The files of a stream as the record has them: each file's tokens without its filters, its
/// filters with where they are declared.
#[derive(Clone, Debug, Default)]
struct Parts {
    tokens: Vec<Vec<Token>>,
    /// The filters of each file: where declared (from the file's start), the filter.
    filters: Vec<Vec<(usize, Filter)>>,
}

/// The tokens of `streams` taken apart into [`Parts`].
fn take_apart(streams: &[Stream]) -> Parts {
    let mut p = Parts::default();
    let mut last = 0u32;
    for st in streams {
        let (mut tokens, mut filters, mut pos) = (Vec::new(), Vec::new(), 0usize);
        for t in &st.tokens {
            match *t {
                Token::Filter(f) => filters.push((pos, f)),
                Token::Lit(_) => pos += 1,
                Token::Copy { len, .. } | Token::Rep { len, .. } => {
                    pos += len as usize;
                    last = len;
                }
                Token::Last => pos += last as usize,
            }
            if !matches!(t, Token::Filter(_)) {
                tokens.push(*t);
            }
        }
        p.tokens.push(tokens);
        p.filters.push(filters);
    }
    p
}

/// The tokens of each file with their filters put back where they are declared.
fn put_together(p: &Parts) -> Result<Vec<Vec<Token>>> {
    let mut last = 0u32;
    let mut out = Vec::new();
    for (tokens, filters) in p.tokens.iter().zip(&p.filters) {
        let mut v = Vec::with_capacity(tokens.len() + filters.len());
        let mut fs = filters.iter().peekable();
        let mut pos = 0usize;
        for t in tokens {
            while let Some(&&(at, f)) = fs.peek() {
                if at > pos {
                    break;
                }
                if at < pos {
                    return Err(Error("a filter declared within a copy"));
                }
                v.push(Token::Filter(f));
                fs.next();
            }
            v.push(*t);
            match *t {
                Token::Lit(_) => pos += 1,
                Token::Copy { len, .. } | Token::Rep { len, .. } => {
                    pos += len as usize;
                    last = len;
                }
                Token::Last => pos += last as usize,
                Token::Filter(_) => {}
            }
        }
        for &(at, f) in fs {
            if at != pos {
                return Err(Error("a filter declared past the file"));
            }
            v.push(Token::Filter(f));
        }
        out.push(v);
    }
    Ok(out)
}

/// The filters of a file declared at `filters`, placed.
fn placed(filters: &[(usize, Filter)]) -> Vec<Placed> {
    filters.iter().map(|&(at, f)| Placed { at: at + f.start as usize, f }).collect()
}

/// The contents `d` of a file filtered again by `placed`, in place: the unfiltered data, where
/// the filters' inverses give it.
fn refilter(d: &mut [u8], placed: &[Placed]) -> Result<()> {
    let mut end = 0;
    for p in placed {
        let (at, len) = (p.at, p.f.len as usize);
        if at < end || at.checked_add(len).is_none_or(|e| e > d.len()) {
            return Err(Error("a filter out of place"));
        }
        end = at + len;
        let r = &mut d[at..end];
        match p.f.kind {
            0 => {
                let c = p.f.channels as usize;
                let src: Vec<u8> = r.to_vec();
                let mut k = 0;
                for ch in 0..c {
                    let mut acc = 0u8;
                    for q in (ch..len).step_by(c) {
                        r[k] = acc.wrapping_sub(src[q]);
                        acc = src[q];
                        k += 1;
                    }
                }
            }
            1 | 2 => {
                let mut i = 0;
                while i + 4 < len {
                    let b = r[i];
                    i += 1;
                    if b == 0xe8 || (p.f.kind == 2 && b == 0xe9) {
                        let pos = ((at + i) as u32) & 0xff_ffff;
                        let v = u32::from_le_bytes(r[i..i + 4].try_into().unwrap());
                        let a = v.wrapping_add(pos);
                        let a = if a < 1 << 24 {
                            a
                        } else {
                            let a = v.wrapping_sub(1 << 24);
                            if a >= 1 << 31 && a.wrapping_add(pos) < 1 << 31 { a } else { v }
                        };
                        r[i..i + 4].copy_from_slice(&a.to_le_bytes());
                        i += 4;
                    }
                }
            }
            _ => {
                let mut i = 0;
                while i + 4 <= len {
                    if r[i + 3] == 0xeb {
                        let v = u32::from_le_bytes([r[i], r[i + 1], r[i + 2], 0]);
                        let v = v.wrapping_add(((at + i) / 4) as u32) & 0xff_ffff;
                        r[i..i + 3].copy_from_slice(&v.to_le_bytes()[..3]);
                    }
                    i += 4;
                }
            }
        }
    }
    Ok(())
}

/// The filters of the files, coded.
fn code_filters(bits: &mut impl Bits, ctx: &mut Ctx, sizes: &[usize], p: &mut Parts, encoding: bool) -> Result<()> {
    bits.cat(Cat::Head);
    let f = &mut ctx.filter;
    for (i, &size) in sizes.iter().enumerate() {
        let given = if encoding { std::mem::take(&mut p.filters[i]) } else { Vec::new() };
        let n = bits.gamma(&mut f[20..52], given.len() as u64)? as usize;
        if n > size {
            return Err(corrupt());
        }
        let (mut kind, mut end, mut decl) = (4usize, 0usize, 0usize);
        let mut out = Vec::with_capacity(n);
        for k in 0..n {
            let (gd, gf) = given
                .get(k)
                .copied()
                .unwrap_or((0, Filter { start: 0, len: 0, kind: 0, channels: 0, start_bytes: 1, len_bytes: 1 }));
            let fk = bits.tree(&mut f[kind * 4..kind * 4 + 4], 2, gf.kind as u32) as u8;
            kind = fk as usize;
            let channels =
                if fk == 0 { bits.gamma(&mut f[52..84], (gf.channels as u64).wrapping_sub(1))? as u8 + 1 } else { 0 };
            let at = gd + gf.start as usize;
            let gap = bits.gamma(&mut f[84..116], at.wrapping_sub(end) as u64)? as usize;
            let at = end.checked_add(gap).filter(|&a| a < size).ok_or_else(corrupt)?;
            let len = if bits.bit(&mut f[212], gf.len == 1 << 16) {
                1 << 16
            } else {
                bits.gamma(&mut f[116..148], gf.len as u64)? as usize
            };
            if len > u32::MAX as usize {
                return Err(corrupt());
            }
            end = at.checked_add(len).filter(|&e| e <= size).ok_or_else(corrupt)?;
            let d = if bits.bit(&mut f[213], gd == decl) {
                decl
            } else {
                let back = bits.gamma(&mut f[148..180], at.wrapping_sub(gd) as u64)? as usize;
                at.checked_sub(back).ok_or_else(corrupt)?
            };
            if d < decl || at - d > u32::MAX as usize {
                return Err(corrupt());
            }
            decl = d;
            let start = (at - d) as u32;
            let sb = if bits.bit(&mut f[214], gf.start_bytes == min_bytes(start)) {
                min_bytes(start)
            } else {
                bits.raws(2, (gf.start_bytes as u64).wrapping_sub(1)) as u8 + 1
            };
            let lb = if bits.bit(&mut f[215], gf.len_bytes == min_bytes(len as u32)) {
                min_bytes(len as u32)
            } else {
                bits.raws(2, (gf.len_bytes as u64).wrapping_sub(1)) as u8 + 1
            };
            out.push((d, Filter { start, len: len as u32, kind: fk, channels, start_bytes: sb, len_bytes: lb }));
        }
        if encoding && out != given {
            return Err(Error("filters coded otherwise"));
        }
        p.filters[i] = out;
        if bits.failed() {
            return Err(corrupt());
        }
    }
    Ok(())
}

/// The bytes where the unfiltered data `u` differs from the contents filtered again `v` (the
/// encoder's), coded and applied to `v`.
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

/// The form of a match: an explicit copy, a repeated distance (its index), the last again.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Form {
    Copy,
    Rep(u8),
    Last,
}

impl Form {
    fn number(self) -> u32 {
        match self {
            Form::Copy => 0,
            Form::Rep(i) => 1 + i as u32,
            Form::Last => 5,
        }
    }
    fn of_number(n: u32) -> Option<Form> {
        match n {
            0 => Some(Form::Copy),
            1..=4 => Some(Form::Rep(n as u8 - 1)),
            5 => Some(Form::Last),
            _ => None,
        }
    }
}

/// The walk over the unfiltered data `u` of the files of `sizes`, coding their tokens
/// (`tokens` given to the encoder, made by the decoder), the matches within `reach`.
fn walk(
    u: &[u8],
    sizes: &[usize],
    reach: usize,
    bits: &mut impl Bits,
    ctx: &mut Ctx,
    tokens: &mut [Vec<Token>],
    encoding: bool,
) -> Result<()> {
    let mut mf = Mf::new(u, reach);
    let (mut pairs, mut next_pairs) = (Vec::new(), Vec::new());
    let mut cache = [0usize; 4];
    let mut last = 0u32;
    let mut fs = 0usize;
    for (fi, &size) in sizes.iter().enumerate() {
        let fe = fs + size;
        let given = if encoding { std::mem::take(&mut tokens[fi]) } else { Vec::new() };
        let mut made = Vec::new();
        let (mut pos, mut prev, mut k) = (fs, 0usize, 0usize);
        let mut ahead = usize::MAX;
        while pos < fe {
            mf.advance(pos);
            let cap = (fe - pos).min(CAP);
            let in_cache = |c: &[usize; 4], d: usize| c.contains(&d);
            let ok = |c: [usize; 4]| move |l: usize, d: usize| l >= 2 + bonus(d) as usize && !c.contains(&d);
            if ahead == pos {
                std::mem::swap(&mut pairs, &mut next_pairs);
            } else {
                mf.pairs(pos, cap, ok(cache), &mut pairs);
            }
            ahead = usize::MAX;
            // the repeated distances' matches
            let mut rl = [0usize; 4];
            for i in 0..4 {
                let d = cache[i];
                if d != 0 && d <= pos && d <= reach {
                    rl[i] = lcp(u, pos - d, pos, cap);
                }
            }
            let rbest = *rl.iter().max().unwrap();
            let rarg = rl.iter().position(|&l| l == rbest).unwrap();
            let longest = pairs.last().copied().unwrap_or((0, 0));
            let cur = longest.0.max(rbest);
            let lazy = if cur >= 2 && pos + 1 < fe {
                mf.advance(pos + 1);
                mf.pairs(pos + 1, (fe - pos - 1).min(CAP), ok(cache), &mut next_pairs);
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
            // the match given: its length, distance and form
            let (g_len, g_dist, g_form) = match g {
                Token::Copy { len, dist } => (len as usize, dist, Form::Copy),
                Token::Rep { idx, len } => (len as usize, cache[idx as usize & 3], Form::Rep(idx)),
                Token::Last => (last as usize, cache[0], Form::Last),
                _ => (0, 0, Form::Copy),
            };
            bits.cat(Cat::Pair);
            let rep = rbest >= 2 && bits.bit(&mut ctx.is_rep[repc * 12 + lb(longest.0)], in_cache(&cache, g_dist));
            let (len, dist) = if rep {
                let gi = cache.iter().position(|&d| d == g_dist).unwrap_or(0) as u32;
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
                    let s =
                        bits.gamma(&mut ctx.rep_shorter[lb(m) * 32..lb(m) * 32 + 32], top.wrapping_sub(g_len) as u64)?;
                    top.checked_sub(s as usize).ok_or_else(corrupt)?
                };
                (len, cache[i])
            } else {
                // the pair from the longest: j steps back; past them all, an escape
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
                            ((pairs[i].0 - pairs[i - 1].0 - 1).min(2), (bl(pairs[i].1) - bl(pairs[i - 1].1)).min(3))
                        }
                    };
                    if !bits.bit(&mut ctx.pair[((j.min(2) * 4 + dl) * 4 + dd) * 2 + (i == 0) as usize], j < gj) {
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
                        let s = bits
                            .gamma(&mut ctx.shorter[lb(top) * 32..lb(top) * 32 + 32], top.wrapping_sub(g_len) as u64)?;
                        top.checked_sub(s as usize).ok_or_else(corrupt)?
                    };
                    (len, dist)
                } else {
                    bits.cat(Cat::Escape);
                    let lc = lb(longest.0);
                    let len = bits.gamma(&mut ctx.escape[lc * 16..lc * 16 + 16], g_len.wrapping_sub(2) as u64)?;
                    let len = (len as usize).checked_add(2).filter(|&l| l <= cap).ok_or_else(corrupt)?;
                    let c2 = cache;
                    let okd = move |d: usize| len >= 2 + bonus(d) as usize && !c2.contains(&d);
                    let rank = if encoding { mf.rank(pos, len, g_dist, okd) } else { 0 };
                    let lc = lb(len);
                    let r = bits.gamma(&mut ctx.rank[lc * 24..lc * 24 + 24], rank as u64)? as usize;
                    let dist = match mf.nth(pos, len, r, okd) {
                        Some(d) => d,
                        None => {
                            (bits.gamma(g32(&mut ctx.misc, 0), g_dist.wrapping_sub(1) as u64)? as usize).wrapping_add(1)
                        }
                    };
                    (len, dist)
                }
            };
            if dist == 0 || dist > pos || dist > reach || len < 2 || len > cap || lcp(u, pos - dist, pos, len) < len {
                return Err(Error("a match that does not fit"));
            }
            // how it is written: RAR's way (the last copy again, else a repeated distance,
            // else explicit) or another
            bits.cat(Cat::Form);
            let pred = if dist == cache[0] && len == last as usize {
                Form::Last
            } else if let Some(i) = cache.iter().position(|&d| d == dist) {
                Form::Rep(i as u8)
            } else {
                Form::Copy
            };
            let form = if pred == Form::Copy {
                Form::Copy
            } else {
                let seg = ((pos - fs).is_multiple_of(SEGMENT) || pos.is_multiple_of(SEGMENT)) as usize;
                let pk = matches!(pred, Form::Rep(_)) as usize;
                if bits.bit(&mut ctx.form[pk * 2 + seg], g_form == pred) {
                    pred
                } else {
                    Form::of_number(bits.tree(&mut ctx.form_alt, 3, g_form.number())).ok_or_else(corrupt)?
                }
            };
            let t = match form {
                Form::Copy => {
                    if len < 2 + bonus(dist) as usize || len - bonus(dist) as usize > 4097 {
                        return Err(corrupt());
                    }
                    cache.copy_within(0..3, 1);
                    cache[0] = dist;
                    last = len as u32;
                    Token::Copy { len: len as u32, dist }
                }
                Form::Rep(i) => {
                    let i = i as usize;
                    if cache[i] != dist || len > 4097 {
                        return Err(corrupt());
                    }
                    cache.copy_within(0..i, 1);
                    cache[0] = dist;
                    last = len as u32;
                    Token::Rep { idx: i as u8, len: len as u32 }
                }
                Form::Last => {
                    if dist != cache[0] || len != last as usize {
                        return Err(corrupt());
                    }
                    Token::Last
                }
            };
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
        if encoding && k != given.len() {
            return Err(Error("tokens past the data"));
        }
        tokens[fi] = made;
        fs = fe;
    }
    Ok(())
}

/// The blocks of the files' tokens `tokens` (with their filters), coded: their sizes against
/// RAR's buffer, whether they send tables, the tables against Huffman's lengths.
fn code_blocks(
    bits: &mut impl Bits,
    ctx: &mut Ctx,
    tokens: Vec<Vec<Token>>,
    streams: Option<&[Stream]>,
    v1: bool,
) -> Result<Vec<Stream>> {
    bits.cat(Cat::Blocks);
    let encoding = streams.is_some();
    // the blocks' sizes and whether they have tables
    let mut blocks: Vec<Vec<(usize, bool)>> = Vec::new();
    for (fi, t) in tokens.iter().enumerate() {
        let given: Vec<(usize, bool)> = if let Some(streams) = streams {
            if streams[fi].tokens != *t {
                return Err(Error("tokens put together otherwise"));
            }
            streams[fi].blocks.iter().map(|b| (b.tokens, b.tables.is_some())).collect()
        } else {
            Vec::new()
        };
        let mut made = Vec::new();
        let mut at = 0;
        loop {
            let first = made.is_empty();
            let room = UNITS - first as usize;
            let (mut k, mut used) = (0, 0);
            while at + k < t.len() && used + units(&t[at + k]) <= room {
                used += units(&t[at + k]);
                k += 1;
            }
            let g = given.get(made.len()).copied().unwrap_or((0, false));
            let n = if bits.bit(&mut ctx.cut[first as usize], g.0 == k) {
                k
            } else {
                bits.gamma(&mut ctx.cut[2..34], g.0 as u64)? as usize
            };
            if at + n > t.len() || (n == 0 && at < t.len()) {
                return Err(corrupt());
            }
            let tc = (first as usize) * 2 + (fi > 0) as usize;
            let tables = bits.bit(&mut ctx.tables[tc], g.1);
            made.push((n, tables));
            at += n;
            // (after the last token, blocks of tables alone may follow)
            if at == t.len() && !bits.bit(&mut ctx.cut[34], given.len() > made.len()) {
                break;
            }
            if bits.failed() || made.len() > t.len() + 64 {
                return Err(corrupt());
            }
        }
        if encoding && made != given {
            return Err(Error("blocks coded otherwise"));
        }
        blocks.push(made);
    }
    // the tables, each over the tokens it serves (up to the next tables, across the files)
    let mut spans: Vec<(usize, usize, usize, usize)> = Vec::new(); // (file, block, from file, from token)
    let mut serve: Vec<Vec<Token>> = Vec::new();
    for (fi, bs) in blocks.iter().enumerate() {
        let mut at = 0;
        for (bi, &(n, tables)) in bs.iter().enumerate() {
            let toks = &tokens[fi][at..at + n];
            if tables {
                spans.push((fi, bi, fi, at));
                serve.push(toks.to_vec());
            } else if let Some(s) = serve.last_mut() {
                s.extend_from_slice(toks);
            }
            at += n;
        }
    }
    let mut made_tables: Vec<Vec<Option<Tables>>> = blocks.iter().map(|bs| vec![None; bs.len()]).collect();
    for ((fi, bi, _, _), toks) in spans.iter().zip(&serve) {
        let given = streams.and_then(|s| s[*fi].blocks[*bi].tables.clone());
        let c = lz5::counts(toks, v1);
        let mut lens = Vec::with_capacity(lz5::lens_len(v1));
        for f in &c {
            lens.extend(huff::lengths(f, huff::MAX));
        }
        let lens = code_lens(bits, ctx, 0, lens, given.as_ref().map(|g| &g.lens[..]))?;
        let mut pc = vec![0u32; lz5::PRE];
        lz5::rle(&lens).iter().for_each(|r| pc[r.0 as usize] += 1);
        let pre = huff::lengths(&pc, huff::MAX);
        let pre = code_lens(bits, ctx, 1, pre, given.as_ref().map(|g| &g.pre[..]))?;
        made_tables[*fi][*bi] = Some(Tables { pre: pre.try_into().unwrap(), lens });
        if bits.failed() {
            return Err(corrupt());
        }
    }
    let mut out = Vec::with_capacity(blocks.len());
    for ((bs, mut tables), tokens) in blocks.iter().zip(made_tables).zip(tokens) {
        let blocks = bs
            .iter()
            .enumerate()
            .map(|(bi, &(n, _))| Block { tables: tables[bi].take(), tokens: n, last: bi + 1 == bs.len() })
            .collect();
        out.push(Stream { blocks, tokens });
    }
    Ok(out)
}

/// Code lengths predicted `pred`, coded as they are (`given`): all as predicted, or each.
fn code_lens(bits: &mut impl Bits, ctx: &mut Ctx, c: usize, pred: Vec<u8>, given: Option<&[u8]>) -> Result<Vec<u8>> {
    if bits.bit(&mut ctx.predicted[c], given.is_none_or(|g| g == &pred[..])) {
        return Ok(pred);
    }
    let mut out = pred;
    for (i, l) in out.iter_mut().enumerate() {
        let g = given.map_or(0, |g| g[i]);
        if !bits.bit(&mut ctx.lens[*l as usize], g == *l) {
            *l = bits.raws(4, g as u64) as u8;
        }
    }
    Ok(out)
}

/// A stream split: its data and record.
pub struct Split {
    /// The files' contents, in turn.
    pub data: Vec<u8>,
    pub record: Vec<u8>,
    /// Bits of the record by [`Cat`].
    pub cost: [f64; CATS],
    /// The largest PPMd model of a new model's block, in MiB (RAR 3.x's streams; 0 for none).
    pub ppmd: u32,
}

/// Splits the stream of the files of `parts` (their packed data in turn, of `sizes` bytes each):
/// version 1's algorithm or not, the dictionary `dict`. Fails where it does not decode, or is
/// not what encoding its parse gives.
pub fn split(parts: &[&[u8]], sizes: &[usize], v1: bool, dict: u64) -> Result<Split> {
    if parts.len() != sizes.len() || parts.is_empty() {
        return Err(corrupt());
    }
    let mut dec = Decoder5::new(v1, dict);
    let mut streams = Vec::new();
    let mut data = Vec::new();
    for (&p, &size) in parts.iter().zip(sizes) {
        let last = dec.last();
        let at = dec.out.len();
        let st = dec.file(p, size)?;
        let mut file = dec.out[at..].to_vec();
        lz5::unfilter(&mut file, &st.placed(last))?;
        data.extend_from_slice(&file);
        streams.push(st);
    }
    let u = dec.out;
    let reach = streams
        .iter()
        .flat_map(|s| &s.tokens)
        .map(|t| if let Token::Copy { dist, .. } = t { *dist } else { 0 })
        .max()
        .unwrap_or(0);
    let mut e = Enc::new();
    let mut ctx = Ctx::new();
    e.gamma(g32(&mut ctx.misc, 1), sizes.len() as u64 - 1)?;
    for &s in sizes {
        e.gamma(g32(&mut ctx.misc, 1), s as u64)?;
    }
    e.gamma(g32(&mut ctx.misc, 1), reach as u64)?;
    let mut p = take_apart(&streams);
    code_filters(&mut e, &mut ctx, sizes, &mut p, true)?;
    let mut v = data.clone();
    refile(&mut v, sizes, &p)?;
    code_fixes(&mut e, &mut ctx, Some(&u), &mut v)?;
    walk(&u, sizes, reach, &mut e, &mut ctx, &mut p.tokens, true)?;
    let tokens = put_together(&p)?;
    let made = code_blocks(&mut e, &mut ctx, tokens, Some(&streams), v1)?;
    // (the stream written again from what the record has)
    let mut enc = Encoder5::new(v1);
    let mut out = Vec::new();
    for (st, &part) in made.iter().zip(parts) {
        let at = out.len();
        enc.file(st, &mut out)?;
        if out[at..] != *part {
            return Err(Error("a stream encoded otherwise"));
        }
    }
    let mut record = vec![v1 as u8];
    record.extend(e.c.finish());
    Ok(Split { data, record, cost: e.cost, ppmd: 0 })
}

/// The files' contents `data` of `sizes` filtered again by the filters of `p`, in place.
fn refile(data: &mut [u8], sizes: &[usize], p: &Parts) -> Result<()> {
    let mut at = 0;
    for (i, &s) in sizes.iter().enumerate() {
        refilter(&mut data[at..at + s], &placed(&p.filters[i]))?;
        at += s;
    }
    Ok(())
}

/// The stream that `data` and `record` rebuild, written onto `out`; the sizes of its files'
/// parts.
pub fn join_onto(data: &[u8], record: &[u8], out: &mut Vec<u8>) -> Result<Vec<usize>> {
    let (&v1, rest) = record.split_first().ok_or_else(corrupt)?;
    if v1 > 1 {
        return Err(corrupt());
    }
    let v1 = v1 == 1;
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
    let mut p = Parts { tokens: vec![Vec::new(); n], filters: vec![Vec::new(); n] };
    code_filters(&mut dec, &mut ctx, &sizes, &mut p, false)?;
    let mut u = data.to_vec();
    refile(&mut u, &sizes, &p)?;
    code_fixes(&mut dec, &mut ctx, None, &mut u)?;
    walk(&u, &sizes, reach, &mut dec, &mut ctx, &mut p.tokens, false)?;
    let tokens = put_together(&p)?;
    let streams = code_blocks(&mut dec, &mut ctx, tokens, None, v1)?;
    let mut enc = Encoder5::new(v1);
    let mut parts = Vec::with_capacity(n);
    for st in &streams {
        let at = out.len();
        enc.file(st, out)?;
        parts.push(out.len() - at);
    }
    Ok(parts)
}

/// [`join_onto`] onto a new vector.
pub fn join(data: &[u8], record: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    join_onto(data, record, &mut out)?;
    Ok(out)
}

/// Bytes of the working memory of a stream of `n` bytes of data, besides its data and record:
/// the unfiltered data, the match finder, the tokens (twice), the streams written.
pub fn memory(n: u64) -> u64 {
    n + Mf::memory(n) + 4 * std::mem::size_of::<Token>() as u64 * n / 2 + n + (1 << 20)
}
