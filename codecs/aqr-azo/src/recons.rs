//! The record of an AZO block's parse given its data (`FORMAT.md`, "The record"), and of a
//! stream's structure.
//!
//! Whether a match starts at a position is coded under contexts of the matches the data offers
//! there; which match, among those that fit: a remembered one, one of the recent distances, or
//! one of the longest matches a hash chain finds, nearest first (ALZip's matches are the nearest
//! of their lengths, and as long as the data there allows).

use aqr_common::arith::{ArithDecoder, ArithEncoder, Bit, Coder};
use aqr_common::varint;

use crate::azo::{self, History, MAX_LENGTH, Token};
use crate::{Error, Result, corrupt};

pub use aqr_common::lz::lcp;

/// Candidates looked at on the chain for the longest matches.
pub const DEPTH: usize = 32;

const NONE: u32 = u32::MAX;

/// The most pairs found at a position: [`DEPTH`] candidates on each key's chain.
pub const PAIRS: usize = DEPTH * KEYS.len();

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

/// The remembered matches that fit at a position, at most (the history's size).
pub const VIABLE: usize = 128;

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
    /// The heads of the chains of each key, 65536 each, one after another.
    head: Vec<u32>,
    /// The previous position of each position on each key's chain, `d.len()` each.
    prev: Vec<u32>,
    /// Positions below this are inserted.
    next: usize,
    found: Vec<(usize, usize)>,
}

impl<'a> Mf<'a> {
    fn new(d: &'a [u8]) -> Self {
        Mf {
            d,
            head: vec![NONE; KEYS.len() << 16],
            prev: vec![NONE; KEYS.len() * d.len()],
            next: 0,
            found: Vec::with_capacity(PAIRS),
        }
    }

    /// Inserts the positions up to `pos`.
    fn advance(&mut self, pos: usize) {
        let d = self.d;
        while self.next < pos {
            let p = self.next;
            for (c, &k) in KEYS.iter().enumerate() {
                if p + k <= d.len() {
                    let h = c << 16 | key(d, p, k);
                    self.prev[c * d.len() + p] = self.head[h];
                    self.head[h] = p as u32;
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
            let mut q = self.head[c << 16 | key(d, pos, k)];
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
                q = self.prev[c * d.len() + p];
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

/// The lengths of the vectors of `Ctx`, in the order of its fields.
pub const CTX_LENS: [usize; 11] = [4 * 12 * 6 * 8 * 3, 4 * 4 * 8, 4 * 8, 4, 64, 4 * 4 * 8, 4, 4, 64, 3 * 4 * 4 * 2, 64];

impl Ctx {
    fn new() -> Self {
        let v = |i: usize| vec![Bit::default(); CTX_LENS[i]];
        Ctx {
            is_match: v(0),
            is_reuse: v(1),
            is_recent: v(2),
            which_recent: v(3),
            rank: v(4),
            is_near: v(5),
            which_near: v(6),
            full: v(7),
            shorter: v(8),
            pair: v(9),
            escape: v(10),
        }
    }
}

/// What codes the record's bits ([`Coder`]: the arithmetic encoder, which writes the value
/// given, or decoder, which returns what it reads), of this crate's errors.
trait Bits: Coder {
    /// [`Coder::gamma32`], failing as corrupt input.
    fn gamma(&mut self, t: &mut [Bit], v: u32) -> Result<u32> {
        self.gamma32(t, v, corrupt)
    }
}

impl<C: Coder> Bits for C {}

/// What takes the tokens the decoder decodes, one by one.
type Sink<'a> = &'a mut dyn FnMut(Token) -> Result<()>;

/// The walk over a block's data coding its tokens (`tokens` given to the encoder; the
/// decoder's are given to `sink`).
fn walk(d: &[u8], bits: &mut impl Bits, tokens: &[Token], sink: Sink, encoding: bool) -> Result<()> {
    let n = d.len();
    let mut mf = Mf::new(d);
    let mut ctx = Ctx::new();
    let mut hist = History::default();
    let mut pairs = Vec::with_capacity(PAIRS);
    let mut next_pairs = Vec::with_capacity(PAIRS);
    let mut viable = Vec::with_capacity(VIABLE);
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
            tokens_push(sink, encoding, Token::Literal)?;
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
                viable.extend((0..VIABLE).filter(|&i| fits(reuse_of(i), hist.lengths[i])));
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
        tokens_push(sink, encoding, t)?;
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

fn tokens_push(sink: Sink, encoding: bool, t: Token) -> Result<()> {
    match encoding {
        true => Ok(()),
        false => sink(t),
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
    walk(data, &mut e, tokens, &mut |_| Ok(()), true)?;
    Ok(e.finish())
}

/// The tokens of the block of `data` that `record` codes.
pub fn tokens_of(data: &[u8], record: &[u8]) -> Result<Vec<Token>> {
    let mut dec = ArithDecoder::new(record);
    let mut tokens = Vec::new();
    walk(
        data,
        &mut dec,
        &[],
        &mut |t| {
            tokens.push(t);
            Ok(())
        },
        false,
    )?;
    Ok(tokens)
}

/// Bytes of what the module holds on WebAssembly (32-bit pointers), checked there by assertions:
/// a stream's data and record, a block's data and record, a pair, a position.
pub mod wasm {
    pub const STREAM: u64 = 16;
    pub const BLOCK: u64 = 16;
    pub const PAIR: u64 = 8;
    pub const WORD: u64 = 4;
}

#[cfg(target_family = "wasm")]
const _: () = {
    assert!(size_of::<(&[u8], &[u8])>() as u64 == wasm::STREAM);
    assert!(size_of::<(&[u8], Option<&[u8]>)>() as u64 == wasm::BLOCK);
    assert!(size_of::<(usize, usize)>() as u64 == wasm::PAIR);
    assert!(size_of::<usize>() as u64 == wasm::WORD);
};

/// The size of the blocks of streams ALZip writes, but for their last, and the largest taken.
pub const BLOCK: usize = 1 << 19;

/// A stream split: its data (the blocks' data with the x86 filter undone: the file itself) and
/// record.
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
    // each block's data (the filter undone) and, where it is coded, its record
    let made = aqr_common::pool::par_map(st.blocks.len(), threads, |i| {
        let b = &st.blocks[i];
        let code = &input[b.code.clone()];
        if b.stored() {
            return Ok((st.x86.then(|| unfiltered(code)).transpose()?, None));
        }
        let blk = azo::decode_block(code, b.size, &mut azo::Model::default())?;
        if azo::encode_block(&blk.data, &blk.tokens, &mut azo::Model::default())? != code {
            return Err(Error("a block encoded otherwise"));
        }
        // (the tokens are of the filter's output, the data kept is the file)
        let r = record_block(&blk.data, &blk.tokens)?;
        let d = if st.x86 { unfiltered(&blk.data)? } else { blk.data };
        Ok((Some(d), Some(r)))
    });
    let mut record = vec![st.x86 as u8];
    varint::put(&mut record, size as u64);
    let mut data = Vec::with_capacity(total);
    for (b, m) in st.blocks.iter().zip(made) {
        // (a stored block's data is its code, but where the filter is undone)
        match m? {
            (d, None) => {
                record.push(1);
                data.extend_from_slice(d.as_deref().unwrap_or(&input[b.code.clone()]));
            }
            (d, Some(r)) => {
                record.push(0);
                varint::put(&mut record, r.len() as u64);
                record.extend_from_slice(&r);
                data.extend_from_slice(&d.ok_or_else(corrupt)?);
            }
        }
    }
    Ok(Split { data, record, used: st.end })
}

/// A block with its x86 filter undone, if the filter gives the block back from it (it does for
/// what the encoder's filter made; any other block is not taken).
fn unfiltered(block: &[u8]) -> Result<Vec<u8>> {
    let mut d = block.to_vec();
    azo::unfilter(&mut d);
    let mut again = d.clone();
    azo::filter(&mut again);
    if again != block {
        return Err(Error("a block the x86 filter does not give back"));
    }
    Ok(d)
}

/// The block that has the data `piece` (with the x86 filter undone, where `x86`) once filtered:
/// a copy only where there is a filter.
fn filtered(x86: bool, piece: &[u8]) -> std::borrow::Cow<'_, [u8]> {
    if !x86 {
        return piece.into();
    }
    let mut d = piece.to_vec();
    azo::filter(&mut d);
    d.into()
}

/// The code of a coded block of data `piece` and record `r`, written onto the end of `out`:
/// the tokens the record decodes are encoded as they come, so nothing is held but the models.
fn code_onto(piece: &[u8], r: &[u8], out: Vec<u8>) -> Result<Vec<u8>> {
    let start = out.len();
    let mut model = azo::Model::default();
    let mut e = azo::BlockEncoder::new(piece, &mut model, azo::Encoder::onto(out));
    walk(piece, &mut ArithDecoder::new(r), &[], &mut |t| e.token(t), false)?;
    let out = e.finish()?;
    // (a block coded as long as its data would be read as stored)
    if out.len() - start == piece.len() {
        return Err(Error("a block coded as long as its data"));
    }
    Ok(out)
}

/// A block's data and, where it is coded, its record.
type Piece<'a> = (&'a [u8], Option<&'a [u8]>);

/// The blocks of a stream's data and record: whether its x86 flag is set, and for each block
/// its data and, where it is coded, its record. The vector is reserved at its size.
fn blocks<'a>(data: &'a [u8], record: &'a [u8]) -> Result<(u8, Vec<Piece<'a>>)> {
    let mut at = 0;
    let &x86 = record.first().ok_or_else(corrupt)?;
    if x86 > 1 {
        return Err(corrupt());
    }
    at += 1;
    let size = usize::try_from(varint::get(record, &mut at).map_err(|_| corrupt())?).map_err(|_| corrupt())?;
    if size == 0 || size > BLOCK {
        return Err(corrupt());
    }
    // each block's data, and its record where it is coded
    let mut blocks = Vec::with_capacity(data.len().div_ceil(size));
    for piece in data.chunks(size) {
        let &kind = record.get(at).ok_or_else(corrupt)?;
        at += 1;
        match kind {
            1 => blocks.push((piece, None)),
            0 => {
                let n = usize::try_from(varint::get(record, &mut at).map_err(|_| corrupt())?).map_err(|_| corrupt())?;
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
    Ok((x86, blocks))
}

/// Whether a stream has the x86 filter, the blocks in all and the coded blocks' sizes of a
/// stream's data and record: what the module's memory plan follows ([`crate::plan`]).
pub fn block_sizes(data: &[u8], record: &[u8]) -> Result<(bool, usize, Vec<usize>)> {
    let (x86, blocks) = blocks(data, record)?;
    Ok((x86 != 0, blocks.len(), blocks.iter().filter(|b| b.1.is_some()).map(|b| b.0.len()).collect()))
}

/// The stream that `record` and `data` rebuild, written onto `out`; its blocks on up to `threads`
/// threads (natively; one after another, written as they are made, on one).
pub fn join_onto(data: &[u8], record: &[u8], out: &mut Vec<u8>, threads: usize) -> Result<()> {
    let (x86, blocks) = blocks(data, record)?;
    out.extend_from_slice(&[azo::VERSION, x86]);
    let header = |n: usize, c: usize| {
        let mut h = [0; azo::BLOCK_HEADER];
        h[..4].copy_from_slice(&(n as u32).to_be_bytes());
        h[4..8].copy_from_slice(&(c as u32).to_be_bytes());
        h[8..].copy_from_slice(&((n ^ c) as u32).to_be_bytes());
        h
    };
    if threads <= 1 || blocks.len() <= 1 || cfg!(target_family = "wasm") {
        for &(piece, r) in &blocks {
            match r {
                None => {
                    out.extend_from_slice(&header(piece.len(), piece.len()));
                    // (filtered where it lies)
                    let at = out.len();
                    out.extend_from_slice(piece);
                    if x86 != 0 {
                        azo::filter(&mut out[at..]);
                    }
                }
                Some(r) => {
                    // (the code written after the room for its header, which is then filled)
                    let f = filtered(x86 != 0, piece);
                    let at = out.len();
                    out.extend_from_slice(&[0; azo::BLOCK_HEADER]);
                    *out = code_onto(&f, r, std::mem::take(out))?;
                    let c = out.len() - at - azo::BLOCK_HEADER;
                    out[at..at + azo::BLOCK_HEADER].copy_from_slice(&header(piece.len(), c));
                }
            }
        }
    } else {
        // (each block's body: the code of a coded one, the filtered data of a stored one)
        let bodies = aqr_common::pool::par_map(blocks.len(), threads, |i| match blocks[i] {
            (piece, None) => Ok(filtered(x86 != 0, piece).into_owned()),
            (piece, Some(r)) => code_onto(&filtered(x86 != 0, piece), r, Vec::new()),
        });
        for (&(piece, _), c) in blocks.iter().zip(bodies) {
            let c = c?;
            out.extend_from_slice(&header(piece.len(), c.len()));
            out.extend_from_slice(&c);
        }
    }
    out.extend_from_slice(&header(0, 0));
    Ok(())
}

/// [`join_onto`] onto a new vector.
pub fn join(data: &[u8], record: &[u8], threads: usize) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    join_onto(data, record, &mut out, threads)?;
    Ok(out)
}
