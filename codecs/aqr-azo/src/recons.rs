//! The record of an AZO block's parse given its data (`FORMAT.md`, "The record"), and of a
//! stream's structure.
//!
//! Whether a match starts at a position is coded under contexts of the matches the data offers
//! there; which match, among those that fit: a remembered one, one of the recent distances, or
//! one of the longest matches a hash chain finds, nearest first (ALZip's matches are the nearest
//! of their lengths, and as long as the data there allows).

use aqr_common::arith::{ArithDecoder, ArithEncoder, Bit};
use aqr_common::varint;

use crate::azo::{self, History, MAX_LENGTH, Token};
use crate::{Error, Result, corrupt};

/// Candidates looked at on the chain for the longest matches.
pub const DEPTH: usize = 32;

const NONE: u32 = u32::MAX;

/// Bucket of a length: itself below 8, else 8-11 by its bit length.
#[inline]
fn lb(l: usize) -> usize {
    if l < 8 { l } else { (5 + (usize::BITS - l.leading_zeros()) as usize).min(11) }
}

/// Bucket of a distance: below 16, 256, 4096, 65536, or beyond.
#[inline]
fn db(d: usize) -> usize {
    (d >= 16) as usize + (d >= 256) as usize + (d >= 4096) as usize + (d >= 65536) as usize
}

/// The length of the common prefix of `d[a..]` and `d[b..]`, `a < b`, up to the end of `d`.
#[inline]
pub fn lcp(d: &[u8], a: usize, b: usize) -> usize {
    let n = d.len() - b;
    let mut i = 0;
    while i + 8 <= n {
        let x = u64::from_le_bytes(d[a + i..a + i + 8].try_into().unwrap())
            ^ u64::from_le_bytes(d[b + i..b + i + 8].try_into().unwrap());
        if x != 0 {
            return i + (x.trailing_zeros() / 8) as usize;
        }
        i += 8;
    }
    while i < n && d[a + i] == d[b + i] {
        i += 1;
    }
    i
}

/// The keys of the match finder's chains: their first bytes, 2 as they are, the others hashed.
pub const KEYS: [usize; 5] = [2, 3, 5, 8, 16];

/// The key of `k` bytes at `p` (`p + k` within the data): 16 bits.
#[inline]
fn key(d: &[u8], p: usize, k: usize) -> usize {
    if k == 2 {
        return d[p] as usize | (d[p + 1] as usize) << 8;
    }
    let mut h = 0u64;
    for &b in &d[p..p + k] {
        h = (h ^ b as u64).wrapping_mul(0x100_0000_01b3);
    }
    (h.wrapping_mul(0x9e37_79b9_7f4a_7c15) >> 48) as usize
}

/// The match finder: every position before the current one, on a chain of each of [`KEYS`].
struct Mf<'a> {
    d: &'a [u8],
    head: Vec<Vec<u32>>,
    prev: Vec<Vec<u32>>,
    /// Positions below this are inserted.
    next: usize,
    found: Vec<(usize, usize)>,
}

impl<'a> Mf<'a> {
    fn new(d: &'a [u8]) -> Self {
        Mf {
            d,
            head: KEYS.iter().map(|_| vec![NONE; 1 << 16]).collect(),
            prev: KEYS.iter().map(|_| vec![NONE; d.len()]).collect(),
            next: 0,
            found: Vec::new(),
        }
    }

    /// Inserts the positions up to `pos`.
    fn advance(&mut self, pos: usize) {
        let d = self.d;
        while self.next < pos {
            let p = self.next;
            for (c, &k) in KEYS.iter().enumerate() {
                if p + k <= d.len() {
                    let h = key(d, p, k);
                    self.prev[c][p] = self.head[c][h];
                    self.head[c][h] = p as u32;
                }
            }
            self.next += 1;
        }
    }

    /// The longest matches at `pos` along the first [`DEPTH`] candidates of each chain, nearest
    /// first: each `(length, distance)` longer than those nearer, at most `cap` long.
    fn pairs(&mut self, pos: usize, cap: usize, out: &mut Vec<(usize, usize)>) {
        out.clear();
        let d = self.d;
        let found = &mut self.found;
        found.clear();
        for (c, &k) in KEYS.iter().enumerate() {
            if pos + k > d.len() || k > cap {
                break;
            }
            let mut q = self.head[c][key(d, pos, k)];
            let mut best = k - 1;
            for _ in 0..DEPTH {
                if q == NONE {
                    break;
                }
                let p = q as usize;
                if d[p + best] == d[pos + best] {
                    let l = lcp(d, p, pos).min(cap);
                    if l > best {
                        best = l;
                        found.push((pos - p, l));
                        if l == cap {
                            break;
                        }
                    }
                }
                q = self.prev[c][p];
            }
            if best == cap {
                break;
            }
        }
        // the nearest of each length
        found.sort_unstable();
        let mut best = 1;
        for &(dist, l) in found.iter() {
            if l > best {
                best = l;
                out.push((l, dist));
            }
        }
    }
}

/// The kinds of tokens: their places in the record's contexts.
const LIT: usize = 0;
const REUSE: usize = 1;
const NEAR: usize = 2;
const FAR: usize = 3;

/// The contexts of a block's record.
struct Ctx {
    /// Whether a match starts: by the kind before, the longest pair's length and distance, and
    /// whether the recent distance 0 fits.
    is_match: Vec<Bit>,
    /// A remembered match: by the kind before, whether one of the recent ones fits.
    is_reuse: Vec<Bit>,
    /// One of the two recent ones: by which fit.
    is_recent: Vec<Bit>,
    which_recent: Vec<Bit>,
    /// The rank among the remembered matches that fit.
    rank: Vec<Bit>,
    /// A recent distance: by which fit.
    is_near: Vec<Bit>,
    which_near: Vec<Bit>,
    /// Whether the length is all the data there allows: by the kind.
    full: Vec<Bit>,
    shorter: Vec<Bit>,
    /// The pair, from the longest: by its index and the count.
    pair: Vec<Bit>,
    /// Escapes: a distance and a length coded as they are.
    escape: Vec<Bit>,
}

impl Ctx {
    fn new() -> Self {
        let v = |n: usize| vec![Bit::default(); n];
        Ctx {
            is_match: v(4 * 12 * 6 * 8 * 3),
            is_reuse: v(4 * 4 * 8),
            is_recent: v(4 * 8),
            which_recent: v(4),
            rank: v(64),
            is_near: v(4 * 4 * 8),
            which_near: v(4),
            full: v(4),
            shorter: v(64),
            pair: v(3 * 4 * 4 * 2),
            escape: v(64),
        }
    }
}

/// What codes the record's bits: the arithmetic encoder (which writes the value given) or
/// decoder (which returns what it reads).
trait Bits {
    fn bit(&mut self, b: &mut Bit, v: bool) -> bool;
    fn raw(&mut self, v: bool) -> bool;
    fn failed(&self) -> bool;

    /// `v` as `x = v + 1`: the bit length of `x` less 1 in unary (adaptive bits of `t`), then
    /// the bits of `x` below its top one, raw.
    fn gamma(&mut self, t: &mut [Bit], v: u32) -> Result<u32> {
        let x = v.wrapping_add(1);
        let n = 31 - x.max(1).leading_zeros();
        let mut k = 0;
        while self.bit(&mut t[(k as usize).min(t.len() - 1)], k < n) {
            k += 1;
            if k > 31 || self.failed() {
                return Err(corrupt());
            }
        }
        let mut y = 1u32;
        for i in (0..k).rev() {
            y = y << 1 | self.raw(x >> i & 1 != 0) as u32;
        }
        Ok(y - 1)
    }
}

impl Bits for ArithEncoder {
    fn bit(&mut self, b: &mut Bit, v: bool) -> bool {
        self.encode(v, b.p1());
        b.update(v);
        v
    }
    fn raw(&mut self, v: bool) -> bool {
        self.encode(v, 1 << 15);
        v
    }
    fn failed(&self) -> bool {
        false
    }
}

impl Bits for ArithDecoder<'_> {
    fn bit(&mut self, b: &mut Bit, _: bool) -> bool {
        let v = self.decode(b.p1());
        b.update(v);
        v
    }
    fn raw(&mut self, _: bool) -> bool {
        self.decode(1 << 15)
    }
    fn failed(&self) -> bool {
        self.overrun()
    }
}

/// The walk over a block's data coding its tokens (`tokens` given to the encoder; the
/// decoder's are pushed onto it).
fn walk(d: &[u8], bits: &mut impl Bits, tokens: &mut Vec<Token>, encoding: bool) -> Result<()> {
    let n = d.len();
    let mut mf = Mf::new(d);
    let mut ctx = Ctx::new();
    let mut hist = History::default();
    let mut pairs = Vec::new();
    let mut next_pairs = Vec::new();
    let mut viable = Vec::new();
    let (mut pos, mut prev, mut k) = (1, LIT, 0);
    // (the position whose pairs `next_pairs` holds, found for the lazy context)
    let mut ahead = 0;
    while pos < n {
        mf.advance(pos);
        let cap = (n - pos).min(MAX_LENGTH as usize);
        if ahead == pos {
            std::mem::swap(&mut pairs, &mut next_pairs);
        } else {
            mf.pairs(pos, cap, &mut pairs);
        }
        let given = if encoding { *tokens.get(k).ok_or_else(corrupt)? } else { Token::Literal };
        k += 1;
        // the match a token stands for here, if its data fits
        let fits = |dist: u32, len: u32| {
            let (dist, len) = (dist as usize, len as usize);
            dist >= 1 && dist <= pos && len >= 2 && len <= cap && lcp(d, pos - dist, pos) >= len
        };
        let near_full = |k: usize| {
            let dist = hist.recent_dist[k] as usize;
            if dist == 0 || dist > pos { 0 } else { lcp(d, pos - dist, pos).min(cap) }
        };
        let near_fit = [near_full(0) >= 2, near_full(1) >= 2];
        let reuse_of = |i: usize| (pos as u32).wrapping_sub(hist.places[i]);
        let recent_fit =
            [0, 1].map(|k| fits(reuse_of(hist.recent_index[k] as usize), hist.lengths[hist.recent_index[k] as usize]));
        let longest = pairs.last().copied().unwrap_or((0, 0));
        // the lengths each kind offers: a recent remembered match, a recent distance, a pair
        let rl = (0..2)
            .filter(|&k| recent_fit[k])
            .map(|k| hist.lengths[hist.recent_index[k] as usize] as usize)
            .max()
            .unwrap_or(0);
        let nl = near_full(0).max(near_full(1));
        let fl = longest.0;
        let order =
            (rl >= fl && rl > 0) as usize | ((nl >= fl && nl > 0) as usize) << 1 | ((rl >= nl && rl > 0) as usize) << 2;
        let most = lb(rl.max(nl).max(fl));
        // (lazy matching: a longer match at the next position)
        let best = rl.max(nl).max(fl);
        let lazy = if best >= 2 && pos + 1 < n {
            mf.advance(pos + 1);
            let cap1 = (n - pos - 1).min(MAX_LENGTH as usize);
            mf.pairs(pos + 1, cap1, &mut next_pairs);
            ahead = pos + 1;
            let d0 = hist.recent_dist[0] as usize;
            let near1 = if d0 >= 1 && d0 <= pos + 1 { lcp(d, pos + 1 - d0, pos + 1).min(cap1) } else { 0 };
            let nx = next_pairs.last().map_or(0, |p| p.0).max(near1);
            (nx > best) as usize + (nx > best + 1) as usize
        } else {
            0
        };
        let c = (((prev * 12 + most) * 6 + if fl == 0 { 5 } else { db(longest.1) }) * 8 + order) * 3 + lazy;
        if !bits.bit(&mut ctx.is_match[c], given != Token::Literal) {
            tokens_push(tokens, encoding, Token::Literal);
            pos += 1;
            prev = LIT;
            continue;
        }
        let rf = recent_fit[0] as usize | (recent_fit[1] as usize) << 1;
        let is_reuse = matches!(given, Token::ReuseRecent(_) | Token::Reuse(_));
        let t = if bits.bit(&mut ctx.is_reuse[(prev * 4 + rf) * 8 + order], is_reuse) {
            if bits.bit(&mut ctx.is_recent[rf * 8 + order], matches!(given, Token::ReuseRecent(_))) {
                let g = if let Token::ReuseRecent(k) = given { k == 1 } else { false };
                Token::ReuseRecent(bits.bit(&mut ctx.which_recent[rf], g) as u8)
            } else {
                // the rank among the remembered matches that fit, or past them all for one that
                // does not (then its place itself)
                viable.clear();
                viable.extend((0..128).filter(|&i| fits(reuse_of(i), hist.lengths[i])));
                let g = if let Token::Reuse(i) = given {
                    viable.iter().position(|&v| v == i as usize).unwrap_or(viable.len()) as u32
                } else {
                    0
                };
                let r = bits.gamma(&mut ctx.rank, g)? as usize;
                let i = match viable.get(r) {
                    Some(&i) => i as u32,
                    None => {
                        let g = if let Token::Reuse(i) = given { i as u32 } else { 0 };
                        (0..7).rev().fold(0, |v, b| v << 1 | bits.raw(g >> b & 1 != 0) as u32)
                    }
                };
                Token::Reuse(i as u8)
            }
        } else {
            let nf = near_fit[0] as usize | (near_fit[1] as usize) << 1;
            let (g_dist, g_len) = match given {
                Token::Near(k, l) => (hist.recent_dist[k as usize & 1], l),
                Token::Far(dist, l) => (dist, l),
                _ => (0, 0),
            };
            if bits.bit(&mut ctx.is_near[(prev * 4 + nf) * 8 + order], matches!(given, Token::Near(..))) {
                let g = if let Token::Near(k, _) = given { k == 1 } else { false };
                let k = bits.bit(&mut ctx.which_near[nf], g) as usize;
                let full = near_full(k) as u32;
                Token::Near(k as u8, code_len(bits, &mut ctx, NEAR, full, g_len)?)
            } else {
                // the pair from the longest: j steps back; past them all, an escape
                let m = pairs.len();
                let gj = pairs.iter().rposition(|&(l, dist)| dist as u32 == g_dist && l as u32 >= g_len);
                let gj = gj.map_or(m, |i| m - 1 - i);
                let mut j = 0;
                while j < m {
                    // (what stepping back gives up: length and distance)
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
                    Token::Far(dist as u32, code_len(bits, &mut ctx, FAR, top as u32, g_len)?)
                } else {
                    let dist = bits.gamma(&mut ctx.escape[..32], g_dist)?;
                    let len = bits.gamma(&mut ctx.escape[32..], g_len)?;
                    Token::Far(dist, len)
                }
            }
        };
        let (dist, len) = hist.apply(pos as u32, t).expect("a match");
        if !fits(dist, len) {
            return Err(Error("a match that does not fit"));
        }
        tokens_push(tokens, encoding, t);
        prev = match t {
            Token::ReuseRecent(_) | Token::Reuse(_) => REUSE,
            Token::Near(..) => NEAR,
            _ => FAR,
        };
        pos += len as usize;
        if bits.failed() {
            return Err(corrupt());
        }
    }
    if encoding && k != tokens.len() {
        return Err(Error("tokens past the data"));
    }
    Ok(())
}

fn tokens_push(tokens: &mut Vec<Token>, encoding: bool, t: Token) {
    if !encoding {
        tokens.push(t);
    }
}

/// A match's length against `full`, what the data allows: whether it is that, else how much
/// shorter.
fn code_len(bits: &mut impl Bits, ctx: &mut Ctx, kind: usize, full: u32, len: u32) -> Result<u32> {
    if bits.bit(&mut ctx.full[kind], len == full) {
        return Ok(full);
    }
    let s = bits.gamma(&mut ctx.shorter, full.wrapping_sub(len).wrapping_sub(1))?;
    full.checked_sub(s + 1).ok_or_else(corrupt)
}

/// The record of the block of `data` parsed as `tokens` (after its first byte).
pub fn record_block(data: &[u8], tokens: &[Token]) -> Result<Vec<u8>> {
    let mut e = ArithEncoder::new(Vec::new());
    walk(data, &mut e, &mut tokens.to_vec(), true)?;
    Ok(e.finish())
}

/// Bytes of the working memory of a block of `n` bytes, besides its data and record: the match
/// finder's heads and chains, the tokens (as their vector grows), the code (likewise), the
/// models.
pub fn block_memory(n: u64) -> u64 {
    let heads = (KEYS.len() << 18) as u64;
    heads + 4 * KEYS.len() as u64 * n + 2 * std::mem::size_of::<Token>() as u64 * n + 2 * n + (1 << 20)
}

/// The tokens of the block of `data` that `record` codes.
pub fn tokens_of(data: &[u8], record: &[u8]) -> Result<Vec<Token>> {
    let mut dec = ArithDecoder::new(record);
    let mut tokens = Vec::new();
    walk(data, &mut dec, &mut tokens, false)?;
    Ok(tokens)
}

/// The size of the blocks of streams ALZip writes, but for their last, and the largest taken.
pub const BLOCK: usize = 1 << 19;

/// A stream split: its data (before the x86 filter, as its blocks decode) and record.
pub struct Split {
    pub data: Vec<u8>,
    pub record: Vec<u8>,
    /// Bytes of the stream.
    pub used: usize,
}

/// Splits the AZO stream at the start of `input`, of at most `limit` bytes of data: its data
/// and its record (`FORMAT.md`, "The record of a stream"), its blocks on up to `threads`
/// threads. Fails where the stream does not decode, or is not what encoding its parse gives.
pub fn split(input: &[u8], limit: usize, threads: usize) -> Result<Split> {
    let st = azo::parse(input)?;
    let total: usize = st.blocks.iter().map(|b| b.size).sum();
    if total > limit {
        return Err(Error("over the limit"));
    }
    let size = st.blocks.first().map_or(BLOCK, |b| b.size);
    for (i, b) in st.blocks.iter().enumerate() {
        let last = i + 1 == st.blocks.len();
        if b.size == 0 || b.size > size || size > BLOCK || (!last && b.size != size) {
            return Err(Error("blocks of other sizes"));
        }
    }
    // each coded block's data and record
    let made = aqr_common::pool::par_map(st.blocks.len(), threads, |i| {
        let b = &st.blocks[i];
        let code = &input[b.code.clone()];
        if b.stored() {
            return Ok(None);
        }
        let blk = azo::decode_block(code, b.size, &mut azo::Model::default())?;
        if azo::encode_block(&blk.data, &blk.tokens, &mut azo::Model::default())? != code {
            return Err(Error("a block encoded otherwise"));
        }
        let r = record_block(&blk.data, &blk.tokens)?;
        Ok(Some((blk.data, r)))
    });
    let mut record = vec![st.x86 as u8];
    varint::put(&mut record, size as u64);
    let mut data = Vec::with_capacity(total);
    for (b, m) in st.blocks.iter().zip(made) {
        match m? {
            None => {
                record.push(1);
                data.extend_from_slice(&input[b.code.clone()]);
            }
            Some((d, r)) => {
                record.push(0);
                varint::put(&mut record, r.len() as u64);
                record.extend_from_slice(&r);
                data.extend_from_slice(&d);
            }
        }
    }
    Ok(Split { data, record, used: st.end })
}

/// The code of a coded block of data `piece` and record `r`.
fn code_of(piece: &[u8], r: &[u8]) -> Result<Vec<u8>> {
    let tokens = tokens_of(piece, r)?;
    let code = azo::encode_block(piece, &tokens, &mut azo::Model::default())?;
    // (a block coded as long as its data would be read as stored)
    if code.len() == piece.len() {
        return Err(Error("a block coded as long as its data"));
    }
    Ok(code)
}

/// The stream that `record` and `data` rebuild, written onto `out`; its blocks on up to `threads`
/// threads (natively; one after another, written as they are made, on one).
pub fn join_onto(data: &[u8], record: &[u8], out: &mut Vec<u8>, threads: usize) -> Result<()> {
    let mut at = 0;
    let &x86 = record.first().ok_or_else(corrupt)?;
    if x86 > 1 {
        return Err(corrupt());
    }
    at += 1;
    let size = varint::get(record, &mut at).map_err(|_| corrupt())? as usize;
    if size == 0 || size > BLOCK {
        return Err(corrupt());
    }
    // each block's data, and its record where it is coded
    let mut blocks = Vec::new();
    for piece in data.chunks(size) {
        let &kind = record.get(at).ok_or_else(corrupt)?;
        at += 1;
        match kind {
            1 => blocks.push((piece, None)),
            0 => {
                let n = varint::get(record, &mut at).map_err(|_| corrupt())? as usize;
                let r = record.get(at..at.checked_add(n).ok_or_else(corrupt)?).ok_or_else(corrupt)?;
                at += n;
                blocks.push((piece, Some(r)));
            }
            _ => return Err(corrupt()),
        }
    }
    if at != record.len() {
        return Err(corrupt());
    }
    out.extend_from_slice(&[azo::VERSION, x86]);
    let block = |out: &mut Vec<u8>, n: usize, code: &[u8]| {
        out.extend_from_slice(&(n as u32).to_be_bytes());
        out.extend_from_slice(&(code.len() as u32).to_be_bytes());
        out.extend_from_slice(&((n ^ code.len()) as u32).to_be_bytes());
        out.extend_from_slice(code);
    };
    if threads <= 1 || blocks.len() <= 1 || cfg!(target_family = "wasm") {
        for &(piece, r) in &blocks {
            match r {
                None => block(out, piece.len(), piece),
                Some(r) => block(out, piece.len(), &code_of(piece, r)?),
            }
        }
    } else {
        let codes = aqr_common::pool::par_map(blocks.len(), threads, |i| match blocks[i] {
            (_, None) => Ok(None),
            (piece, Some(r)) => code_of(piece, r).map(Some),
        });
        for (&(piece, _), c) in blocks.iter().zip(codes) {
            match c? {
                None => block(out, piece.len(), piece),
                Some(c) => block(out, piece.len(), &c),
            }
        }
    }
    block(out, 0, &[]);
    Ok(())
}

/// [`join_onto`] onto a new vector.
pub fn join(data: &[u8], record: &[u8], threads: usize) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    join_onto(data, record, &mut out, threads)?;
    Ok(out)
}
