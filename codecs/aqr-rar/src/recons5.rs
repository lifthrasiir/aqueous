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
use crate::rec::{Bits, CATS, Cat, Dec, Enc, Mf, bits, lcp_up};
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

/// The lengths of the vectors of [`Ctx`], in the order of its fields.
pub const CTX_LENS: [usize; 18] = [
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
    2 * 2,
    8,
    2 + 32 + 1,
    4,
    2,
    16,
    5 * 4 + 32 * 6 + 8,
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
            cut: v(12),
            tables: v(13),
            predicted: v(14),
            lens: v(15),
            filter: v(16),
            misc: v(17),
        }
    }
}

/// Bytes of what the module holds on WebAssembly (32-bit pointers), checked there by assertions:
/// a stream's data and record, a token, a filter with where it is declared, a block's tokens and
/// whether it has tables, a pair, an adaptive bit, a code.
pub mod wasm {
    pub const STREAM: u64 = 16;
    pub const TOKEN: u64 = 16;
    pub const FILTER: u64 = 16;
    pub const BLOCK: u64 = 8;
    pub const PAIR: u64 = 8;
    pub const BIT: u64 = 4;
    pub const CODE: u64 = 4;
}

#[cfg(target_family = "wasm")]
const _: () = {
    assert!(size_of::<(&[u8], &[u8])>() as u64 == wasm::STREAM);
    assert!(size_of::<Token>() as u64 == wasm::TOKEN);
    assert!(size_of::<(usize, Filter)>() as u64 == wasm::FILTER);
    assert!(size_of::<(usize, bool)>() as u64 == wasm::BLOCK);
    assert!(size_of::<(usize, usize)>() as u64 == wasm::PAIR);
    assert!(size_of::<Bit>() as u64 == wasm::BIT);
    assert!(size_of::<(u16, u8)>() as u64 == wasm::CODE);
};

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

/// The contents `d` of a file filtered again by `filters` (where each is declared, the filter),
/// in place: the unfiltered data, where the filters' inverses give it. The delta filters copy
/// their bytes, no more than `delta` of them.
fn refilter(d: &mut [u8], filters: &[(usize, Filter)], delta: usize) -> Result<()> {
    let mut end = 0;
    for &(decl, f) in filters {
        let (at, len) = (decl + f.start as usize, f.len as usize);
        if at < end || at.checked_add(len).is_none_or(|e| e > d.len()) {
            return Err(Error("a filter out of place"));
        }
        end = at + len;
        let r = &mut d[at..end];
        let p = Placed { at, f };
        match p.f.kind {
            0 => {
                if len > delta {
                    return Err(corrupt());
                }
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

/// The filters of the files, coded: those of each file after the last one's in `list` (where
/// each is declared, the filter; room for them all reserved), `ends` the end of each file's.
/// The encoder's `given` are the filters of each file.
fn code_filters(
    bits: &mut impl Bits,
    ctx: &mut Ctx,
    sizes: &[usize],
    given: &[Vec<(usize, Filter)>],
    list: &mut Vec<(usize, Filter)>,
    ends: &mut Vec<usize>,
) -> Result<()> {
    bits.cat(Cat::Head);
    let f = &mut ctx.filter;
    let encoding = !given.is_empty();
    let none = Filter { start: 0, len: 0, kind: 0, channels: 0, start_bytes: 1, len_bytes: 1 };
    for (i, &size) in sizes.iter().enumerate() {
        let given = if encoding { &given[i][..] } else { &[] };
        let first = list.len();
        let n = bits.gamma(&mut f[20..52], given.len() as u64)? as usize;
        if n > size || n > list.capacity() - first {
            return Err(corrupt());
        }
        let (mut kind, mut end, mut decl) = (4usize, 0usize, 0usize);
        for k in 0..n {
            let (gd, gf) = given.get(k).copied().unwrap_or((0, none));
            let fk = bits.tree(&mut f[kind * 4..kind * 4 + 4], 2, gf.kind as u32) as u8;
            kind = fk as usize;
            let channels = if fk == 0 {
                let c = bits.gamma(&mut f[52..84], (gf.channels as u64).wrapping_sub(1))?;
                if c >= 32 {
                    return Err(corrupt());
                }
                c as u8 + 1
            } else {
                0
            };
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
            list.push((d, Filter { start, len: len as u32, kind: fk, channels, start_bytes: sb, len_bytes: lb }));
        }
        if encoding && list[first..] != *given {
            return Err(Error("filters coded otherwise"));
        }
        ends.push(list.len());
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

/// The walk over the unfiltered data `u` of the files of `sizes`, coding their tokens (those of
/// each file `given` to the encoder, none to the decoder), the matches within `reach`. The tokens
/// are put onto the end of `out` (room for them reserved) with the files' filters (`filters`, in
/// turn, `fends` the end of each file's) where they are declared, `ends` the end of each file's.
#[allow(clippy::too_many_arguments)]
fn walk(
    u: &[u8],
    sizes: &[usize],
    reach: usize,
    bits: &mut impl Bits,
    ctx: &mut Ctx,
    given: &[Vec<Token>],
    (filters, fends): (&[(usize, Filter)], &[usize]),
    out: &mut Vec<Token>,
    ends: &mut Vec<usize>,
) -> Result<()> {
    let encoding = !given.is_empty();
    let mut mf = Mf::new(u, reach);
    let (mut pairs, mut next_pairs) = (Vec::with_capacity(Mf::PAIRS), Vec::with_capacity(Mf::PAIRS));
    let mut cache = [0usize; 4];
    let mut last = 0u32;
    let mut fs = 0usize;
    for (fi, &size) in sizes.iter().enumerate() {
        let fe = fs + size;
        let given = if encoding { &given[fi][..] } else { &[] };
        let mut fl = &filters[if fi == 0 { 0 } else { fends[fi - 1] }..fends[fi]];
        let (mut pos, mut prev, mut k) = (fs, 0usize, 0usize);
        let mut ahead = usize::MAX;
        while pos < fe {
            // (the filters declared here; one declared within a copy is past)
            while let Some(&(at, f)) = fl.first() {
                if at > pos - fs {
                    break;
                }
                if at < pos - fs {
                    return Err(Error("a filter declared within a copy"));
                }
                push(out, Token::Filter(f))?;
                fl = &fl[1..];
            }
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
                    rl[i] = lcp_up(u, pos - d, pos, cap);
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
                push(out, Token::Lit(u[pos]))?;
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
            if dist == 0 || dist > pos || dist > reach || len < 2 || len > cap || lcp_up(u, pos - dist, pos, len) < len
            {
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
            push(out, t)?;
            pos += len;
            prev = 1;
            if bits.failed() {
                return Err(corrupt());
            }
        }
        if encoding && k != given.len() {
            return Err(Error("tokens past the data"));
        }
        // (the filters declared at the end)
        for &(at, f) in fl {
            if at != size {
                return Err(Error("a filter declared past the file"));
            }
            push(out, Token::Filter(f))?;
        }
        ends.push(out.len());
        fs = fe;
    }
    Ok(())
}

/// `t` pushed onto `out`, which must have room reserved for it.
#[inline]
fn push(out: &mut Vec<Token>, t: Token) -> Result<()> {
    if out.len() == out.capacity() {
        return Err(Error("more tokens than the record counts"));
    }
    out.push(t);
    Ok(())
}

/// The blocks of the files' tokens `tokens` (with their filters; `ends` the end of each file's),
/// coded: their sizes against RAR's buffer, whether they send tables. The blocks are put onto
/// the end of `blocks` (room for them reserved) with the end of each file's in `bends`; the
/// encoder gives the `streams`.
fn code_blocks(
    bits: &mut impl Bits,
    ctx: &mut Ctx,
    tokens: &[Token],
    ends: &[usize],
    streams: Option<&[Stream]>,
    blocks: &mut Vec<(usize, bool)>,
    bends: &mut Vec<usize>,
) -> Result<()> {
    bits.cat(Cat::Blocks);
    let mut fs = 0;
    for (fi, &fe) in ends.iter().enumerate() {
        let t = &tokens[fs..fe];
        let given: &[Block] = match streams {
            Some(streams) => {
                if streams[fi].tokens != *t {
                    return Err(Error("tokens put together otherwise"));
                }
                &streams[fi].blocks
            }
            None => &[],
        };
        let from = blocks.len();
        let mut at = 0;
        loop {
            let made = blocks.len() - from;
            let first = made == 0;
            let room = UNITS - first as usize;
            let (mut k, mut used) = (0, 0);
            while at + k < t.len() && used + units(&t[at + k]) <= room {
                used += units(&t[at + k]);
                k += 1;
            }
            let g = given.get(made).map_or((0, false), |b| (b.tokens, b.tables.is_some()));
            let n = if bits.bit(&mut ctx.cut[first as usize], g.0 == k) {
                k
            } else {
                bits.gamma(&mut ctx.cut[2..34], g.0 as u64)? as usize
            };
            if at + n > t.len() || (n == 0 && at < t.len()) || blocks.len() == blocks.capacity() {
                return Err(corrupt());
            }
            let tc = (first as usize) * 2 + (fi > 0) as usize;
            let tables = bits.bit(&mut ctx.tables[tc], g.1);
            blocks.push((n, tables));
            at += n;
            // (after the last token, blocks of tables alone may follow)
            if at == t.len() && !bits.bit(&mut ctx.cut[34], given.len() > made + 1) {
                break;
            }
            if bits.failed() || made > t.len() + 64 {
                return Err(corrupt());
            }
        }
        if streams.is_some() && !blocks[from..].iter().zip(given).all(|(m, g)| *m == (g.tokens, g.tables.is_some()))
            || blocks.len() - from != given.len() && streams.is_some()
        {
            return Err(Error("blocks coded otherwise"));
        }
        bends.push(blocks.len());
        fs = fe;
    }
    Ok(())
}

/// The tables of a block over the tokens `serve` (those it serves: up to the next tables, across
/// the files), coded: the lengths of Huffman's construction, or as they are (`given`).
fn code_tables(
    bits: &mut impl Bits,
    ctx: &mut Ctx,
    serve: &[Token],
    v1: bool,
    given: Option<&Tables>,
) -> Result<Tables> {
    let c = lz5::counts(serve, v1);
    let mut lens = Vec::with_capacity(lz5::lens_len(v1));
    for f in &c {
        lens.extend(huff::lengths(f, huff::MAX));
    }
    let lens = code_lens(bits, ctx, 0, lens, given.map(|g| &g.lens[..]))?;
    let mut pc = vec![0u32; lz5::PRE];
    lz5::rle(&lens).iter().for_each(|r| pc[r.0 as usize] += 1);
    let pre = huff::lengths(&pc, huff::MAX);
    let pre = code_lens(bits, ctx, 1, pre, given.map(|g| &g.pre[..]))?;
    if bits.failed() {
        return Err(corrupt());
    }
    Ok(Tables { pre: pre.try_into().unwrap(), lens })
}

/// Bytes the tables' work takes at most (the counts, the lengths, the heap, the run-length
/// coding, the tables): with room for the vectors that grow, and for the holes that leaves. The
/// decoder takes them from the stack once before the blocks and gives them back, so that what it
/// takes does not depend on the data.
pub const SCRATCH: usize = 64 << 10;

/// The blocks of the files written onto the end of `out`: each block after its tables, if it
/// sends any (coded as they come: `streams` are the encoder's), in the order of the stream. The
/// end of each file's part in `out` is put in `parts` if given.
#[allow(clippy::too_many_arguments)]
fn write_blocks(
    bits: &mut impl Bits,
    ctx: &mut Ctx,
    enc: &mut Encoder5,
    tokens: &[Token],
    (blocks, bends): (&[(usize, bool)], &[usize]),
    streams: Option<&[Stream]>,
    out: &mut Vec<u8>,
    mut parts: Option<&mut Vec<usize>>,
) -> Result<()> {
    if streams.is_none() {
        std::hint::black_box(Vec::<u8>::with_capacity(SCRATCH));
    }
    let (mut at, mut bi) = (0, 0);
    for (fi, &bend) in bends.iter().enumerate() {
        let from = bi;
        while bi < bend {
            let (n, has) = blocks[bi];
            let tables = if has {
                // the tokens it serves, up to the next tables
                let (mut end, mut j) = (at + n, bi + 1);
                while j < blocks.len() && !blocks[j].1 {
                    end += blocks[j].0;
                    j += 1;
                }
                let given = streams.and_then(|s| s[fi].blocks[bi - from].tables.as_ref());
                Some(code_tables(bits, ctx, &tokens[at..end], enc.v1, given)?)
            } else {
                None
            };
            enc.block(&tokens[at..at + n], tables.as_ref(), bi + 1 == bend, out)?;
            at += n;
            bi += 1;
        }
        if let Some(p) = parts.as_mut() {
            p.push(out.len());
        }
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

/// What a record starts with: the sizes of the stream's files, its reach, and the counts the
/// decoder reserves (and the walk must make exactly): tokens (filters not counted), filters,
/// blocks, and the most bytes of a delta filter.
pub struct Start {
    pub v1: bool,
    pub sizes: Vec<usize>,
    pub reach: usize,
    pub tokens: usize,
    pub filters: usize,
    pub blocks: usize,
    pub delta: usize,
}

/// The start of `record` for a stream of `n` bytes of data, with the decoder and the contexts as
/// they are after it.
fn start<'a>(record: &'a [u8], n: usize, ctx: &mut Ctx) -> Result<(Start, Dec<'a>)> {
    let (&v1, rest) = record.split_first().ok_or_else(corrupt)?;
    if v1 > 1 {
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
    // (a token covers a byte at least, a file has fewer filters than bytes; blocks are bounded as
    // `code_blocks` bounds them)
    let mut count = |i: usize, most: usize| -> Result<usize> {
        let v = dec.gamma(g32(&mut ctx.misc, i), 0)? as usize;
        if v > most {
            return Err(corrupt());
        }
        Ok(v)
    };
    let tokens = count(4, n)?;
    let filters = count(5, n)?;
    let blocks = count(6, tokens + filters + 64 * files)?;
    let delta = count(7, n)?;
    Ok((Start { v1: v1 == 1, sizes, reach, tokens, filters, blocks, delta }, dec))
}

/// The start of `record` for a stream of `n` bytes of data: what the memory plan follows
/// ([`crate::plan`]).
pub fn start_of(record: &[u8], n: usize) -> Result<Start> {
    Ok(start(record, n, &mut Ctx::new())?.0)
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
    let mut e = Enc::new(Cat::Head as usize);
    let mut ctx = Ctx::new();
    e.gamma(g32(&mut ctx.misc, 1), sizes.len() as u64 - 1)?;
    for &s in sizes {
        e.gamma(g32(&mut ctx.misc, 1), s as u64)?;
    }
    e.gamma(g32(&mut ctx.misc, 1), reach as u64)?;
    let p = take_apart(&streams);
    let tokens = p.tokens.iter().map(Vec::len).sum::<usize>();
    let filters = p.filters.iter().map(Vec::len).sum::<usize>();
    let blocks = streams.iter().map(|s| s.blocks.len()).sum::<usize>();
    let delta =
        (p.filters.iter().flatten()).filter(|(_, f)| f.kind == 0).map(|(_, f)| f.len as usize).max().unwrap_or(0);
    for (i, v) in [(4, tokens), (5, filters), (6, blocks), (7, delta)] {
        e.gamma(g32(&mut ctx.misc, i), v as u64)?;
    }
    let (mut flist, mut fends) = (Vec::with_capacity(filters), Vec::new());
    code_filters(&mut e, &mut ctx, sizes, &p.filters, &mut flist, &mut fends)?;
    let mut v = data.clone();
    refile(&mut v, sizes, &flist, &fends, delta)?;
    code_fixes(&mut e, &mut ctx, Some(&u), &mut v)?;
    let (mut merged, mut ends) = (Vec::with_capacity(tokens + filters), Vec::new());
    walk(&u, sizes, reach, &mut e, &mut ctx, &p.tokens, (&flist, &fends), &mut merged, &mut ends)?;
    let (mut bl, mut bends) = (Vec::with_capacity(blocks), Vec::new());
    code_blocks(&mut e, &mut ctx, &merged, &ends, Some(&streams), &mut bl, &mut bends)?;
    // (the stream written again from what the record has)
    let mut enc = Encoder5::new(v1);
    let (mut out, mut ends_out) = (Vec::new(), Vec::new());
    write_blocks(&mut e, &mut ctx, &mut enc, &merged, (&bl, &bends), Some(&streams), &mut out, Some(&mut ends_out))?;
    let mut at = 0;
    for (&end, &part) in ends_out.iter().zip(parts) {
        if out[at..end] != *part {
            return Err(Error("a stream encoded otherwise"));
        }
        at = end;
    }
    let mut record = vec![v1 as u8];
    record.extend(e.c.finish());
    Ok(Split { data, record, cost: e.cost, ppmd: 0 })
}

/// The files' contents `data` of `sizes` filtered again by the filters `list` (`ends` the end of
/// each file's), in place.
fn refile(data: &mut [u8], sizes: &[usize], list: &[(usize, Filter)], ends: &[usize], delta: usize) -> Result<()> {
    let (mut at, mut from) = (0, 0);
    for (i, &s) in sizes.iter().enumerate() {
        refilter(&mut data[at..at + s], &list[from..ends[i]], delta)?;
        (at, from) = (at + s, ends[i]);
    }
    Ok(())
}

/// The stream that `data` and `record` rebuild, written onto `out` (room for it reserved).
pub fn join_onto(data: &[u8], record: &[u8], out: &mut Vec<u8>) -> Result<()> {
    let mut ctx = Ctx::new();
    let (st, mut dec) = start(record, data.len(), &mut ctx)?;
    let Start { v1, sizes, reach, tokens, filters, blocks, delta } = st;
    let files = sizes.len();
    let mut u = data.to_vec();
    let (mut flist, mut fends) = (Vec::with_capacity(filters), Vec::with_capacity(files));
    code_filters(&mut dec, &mut ctx, &sizes, &[], &mut flist, &mut fends)?;
    if flist.len() != filters {
        return Err(Error("a count of filters that does not fit"));
    }
    refile(&mut u, &sizes, &flist, &fends, delta)?;
    code_fixes(&mut dec, &mut ctx, None, &mut u)?;
    // (the tokens and the filters among them reserved: their counts are the record's)
    let (mut merged, mut ends) = (Vec::with_capacity(tokens + filters), Vec::with_capacity(files));
    walk(&u, &sizes, reach, &mut dec, &mut ctx, &[], (&flist, &fends), &mut merged, &mut ends)?;
    if merged.len() != tokens + filters {
        return Err(Error("a count of tokens that does not fit"));
    }
    let (mut bl, mut bends) = (Vec::with_capacity(blocks), Vec::with_capacity(files));
    code_blocks(&mut dec, &mut ctx, &merged, &ends, None, &mut bl, &mut bends)?;
    if bl.len() != blocks {
        return Err(Error("a count of blocks that does not fit"));
    }
    let mut enc = Encoder5::new(v1);
    write_blocks(&mut dec, &mut ctx, &mut enc, &merged, (&bl, &bends), None, out, None)
}

/// [`join_onto`] onto a new vector.
pub fn join(data: &[u8], record: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    join_onto(data, record, &mut out)?;
    Ok(out)
}
