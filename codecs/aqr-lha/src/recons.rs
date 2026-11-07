//! The record of an `-lh4-` to `-lh7-` stream given its data (`FORMAT.md`, "The record"): its
//! parse, its blocks' sizes and headers, its end.
//!
//! Whether a match starts at a position is coded under contexts of the matches the data offers
//! there; which match, as one of the longest matches hash chains find, nearest first (the
//! compressors' matches are mostly the nearest of their lengths, as long as the data allows).
//! A block's size is coded against the cut of a compressor buffering its tokens (a byte for a
//! literal, three for a match, one for each eight tokens), its threshold learnt from the cuts
//! before; its header against the one [`huff::head`] writes for its tokens.

use aqr_common::arith::{ArithDecoder, Bit, Coder, Costed};

use crate::huff;
use crate::lzh::{self, Head, MAX_MATCH, MIN_MATCH, Method, Stream, Token};
use crate::{Error, Result, corrupt};

pub use aqr_common::lz::lcp;

/// Bytes of what the module holds on WebAssembly (32-bit pointers), checked there by assertions:
/// a stream's data and record, a token, a pair, a node's link.
pub mod wasm {
    pub const STREAM: u64 = 16;
    pub const TOKEN: u64 = 8;
    pub const PAIR: u64 = 8;
}

#[cfg(target_family = "wasm")]
const _: () = {
    assert!(size_of::<(&[u8], &[u8])>() as u64 == wasm::STREAM);
    assert!(size_of::<Token>() as u64 == wasm::TOKEN);
    assert!(size_of::<(usize, usize)>() as u64 == wasm::PAIR);
};

/// Candidates looked at on each chain for the longest matches.
pub const DEPTH: usize = 32;

const NONE: u32 = u32::MAX;
/// Positions held by the chains: more than the farthest reach.
const RING: usize = 1 << 17;

/// Bucket of a length: itself below 8 (3-7), else 8-11 by its bit length.
#[inline]
fn lb(l: usize) -> usize {
    if l < 8 { l } else { (5 + (usize::BITS - l.leading_zeros()) as usize).min(11) }
}

/// Bucket of a distance: below 16, 256, 1024, 4096, 16384, or beyond.
#[inline]
fn db(d: usize) -> usize {
    (d >= 16) as usize + (d >= 256) as usize + (d >= 1024) as usize + (d >= 4096) as usize + (d >= 16384) as usize
}

/// The keys of the match finder's chains: their first bytes, hashed.
pub const KEYS: [usize; 4] = [3, 5, 8, 16];

/// The key of `k` bytes at `p` (`p + k` within the data): 16 bits.
#[inline]
fn key(d: &[u8], p: usize, k: usize) -> usize {
    let mut h = 0u64;
    for &b in &d[p..p + k] {
        h = (h ^ b as u64).wrapping_mul(0x100_0000_01b3);
    }
    (h.wrapping_mul(0x9e37_79b9_7f4a_7c15) >> 48) as usize
}

/// The match finder: the positions before the current one within reach, on a chain of each of
/// [`KEYS`].
struct Mf<'a> {
    d: &'a [u8],
    reach: usize,
    /// The heads of the chains of each key, 65536 each, one after another.
    head: Vec<u32>,
    /// The previous position on each key's chain, of the positions within a ring of `mask + 1`
    /// slots, one ring after another.
    prev: Vec<u32>,
    mask: usize,
    /// Positions below this are inserted.
    next: usize,
    found: Vec<(usize, usize)>,
}

/// Slots of the rings of the chains for a stream of `n` bytes.
pub fn ring_slots(n: usize) -> usize {
    RING.min(n.next_power_of_two())
}

/// Slots of the tree's nodes for a stream of `n` bytes within `reach`.
pub fn tree_slots(n: usize, reach: usize) -> usize {
    (reach + 1).next_power_of_two().min(n.next_power_of_two().max(1))
}

/// The most pairs found at a position: [`DEPTH`] candidates on each key's chain.
pub const PAIRS: usize = DEPTH * KEYS.len();

impl<'a> Mf<'a> {
    fn new(d: &'a [u8], reach: usize) -> Self {
        Mf {
            d,
            reach,
            head: vec![NONE; KEYS.len() << 16],
            prev: vec![NONE; KEYS.len() * ring_slots(d.len())],
            mask: ring_slots(d.len()) - 1,
            next: 0,
            found: Vec::with_capacity(PAIRS),
        }
    }

    /// Inserts the positions up to `pos`.
    fn advance(&mut self, pos: usize) {
        let d = self.d;
        let mask = self.mask;
        while self.next < pos {
            let p = self.next;
            for (c, &k) in KEYS.iter().enumerate() {
                if p + k <= d.len() {
                    let h = c << 16 | key(d, p, k);
                    self.prev[c * (mask + 1) + (p & mask)] = self.head[h];
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
        let mask = self.mask;
        let found = &mut self.found;
        found.clear();
        for (c, &k) in KEYS.iter().enumerate() {
            if pos + k > d.len() || k > cap {
                break;
            }
            let mut q = self.head[c << 16 | key(d, pos, k)];
            let mut best = k - 1;
            for _ in 0..DEPTH {
                if q == NONE || pos - q as usize > self.reach {
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
                q = self.prev[c * (mask + 1) + (p & mask)];
            }
            if best == cap {
                break;
            }
        }
        // the nearest of each length
        found.sort_unstable();
        let mut best = MIN_MATCH - 1;
        for &(dist, l) in found.iter() {
            if l > best {
                best = l;
                out.push((l, dist));
            }
        }
    }
}

/// The binary search tree of the positions within reach, by the 256 bytes from each (the first
/// byte choosing one of 256 trees): the match finder of LZSS as Okumura wrote it, which LHA's
/// compressors of LHarc's lineage keep. Every position is inserted; the match it finds is the
/// first node on the path down of the longest length, and a node matching in full is replaced
/// by the newer position.
struct Bst<'a> {
    d: &'a [u8],
    reach: usize,
    mask: usize,
    /// Children and parents of the nodes: positions' slots, then the 256 roots, then none.
    ls: Vec<u32>,
    rs: Vec<u32>,
    dad: Vec<u32>,
    /// Positions below this are inserted.
    next: usize,
    /// The match found when inserting the last position: length and distance.
    last: (usize, usize),
}

/// Bytes the tree compares.
const FULL: usize = MAX_MATCH;

impl<'a> Bst<'a> {
    fn new(d: &'a [u8], reach: usize) -> Self {
        let r = tree_slots(d.len(), reach);
        let nil = (r + 256) as u32;
        Bst {
            d,
            reach,
            mask: r - 1,
            ls: vec![nil; r + 257],
            rs: vec![nil; r + 257],
            dad: vec![nil; r + 257],
            next: 0,
            last: (0, 0),
        }
    }

    fn nil(&self) -> u32 {
        (self.ls.len() - 1) as u32
    }

    /// Takes the node of position `q` out.
    fn delete(&mut self, q: usize) {
        let nil = self.nil();
        let p = (q & self.mask) as u32;
        let (ls, rs, dad) = (&mut self.ls, &mut self.rs, &mut self.dad);
        let pu = p as usize;
        if dad[pu] == nil {
            return;
        }
        let q = if rs[pu] == nil {
            ls[pu]
        } else if ls[pu] == nil {
            rs[pu]
        } else {
            let mut q = ls[pu];
            if rs[q as usize] != nil {
                while rs[q as usize] != nil {
                    q = rs[q as usize];
                }
                let qd = dad[q as usize] as usize;
                rs[qd] = ls[q as usize];
                dad[ls[q as usize] as usize] = qd as u32;
                ls[q as usize] = ls[pu];
                dad[ls[pu] as usize] = q;
            }
            rs[q as usize] = rs[pu];
            dad[rs[pu] as usize] = q;
            q
        };
        dad[q as usize] = dad[pu];
        let pd = dad[pu] as usize;
        if rs[pd] == p {
            rs[pd] = q;
        } else {
            ls[pd] = q;
        }
        dad[pu] = nil;
    }

    /// Inserts position `r`, finding its match.
    fn insert(&mut self, r: usize) {
        let d = self.d;
        let n = d.len();
        let nil = self.nil();
        let s = (r & self.mask) as u32;
        let su = s as usize;
        self.ls[su] = nil;
        self.rs[su] = nil;
        let mut p = (self.mask + 1 + d[r] as usize) as u32;
        let mut cmp = 1i32;
        let (mut best, mut best_at) = (0usize, 0usize);
        // (a slot's position: the one of its slot within reach before `r`)
        let at = |slot: u32| r - ((r - slot as usize) & self.mask);
        loop {
            let pu = p as usize;
            let child = if cmp >= 0 { &mut self.rs } else { &mut self.ls };
            if child[pu] == nil {
                child[pu] = s;
                self.dad[su] = p;
                self.last = if best >= MIN_MATCH { (best, r - best_at) } else { (0, 0) };
                return;
            }
            p = child[pu];
            let q = at(p);
            let mut i = 1;
            while i < FULL {
                if r + i >= n {
                    cmp = -1;
                    break;
                }
                cmp = d[r + i] as i32 - d[q + i] as i32;
                if cmp != 0 {
                    break;
                }
                i += 1;
            }
            if i > best {
                best = i;
                best_at = q;
                if i >= FULL {
                    break;
                }
            }
        }
        // the node of `q` replaced by `r`'s
        let pu = p as usize;
        let (ls, rs, dad) = (&mut self.ls, &mut self.rs, &mut self.dad);
        dad[su] = dad[pu];
        ls[su] = ls[pu];
        rs[su] = rs[pu];
        dad[ls[pu] as usize] = s;
        dad[rs[pu] as usize] = s;
        let pd = dad[pu] as usize;
        if rs[pd] == p {
            rs[pd] = s;
        } else {
            ls[pd] = s;
        }
        dad[pu] = nil;
        self.last = (best, r - best_at);
    }

    /// Inserts the positions up to `pos`, the match found at `pos` then in `last`.
    fn advance(&mut self, pos: usize) {
        while self.next <= pos {
            let r = self.next;
            if r > self.reach {
                self.delete(r - self.reach - 1);
            }
            self.insert(r);
            self.next += 1;
        }
    }
}

impl Mf<'_> {
    /// The rank of the match `dist` back among those of at least `len` bytes on the chain of 3
    /// bytes at `pos`, nearest first, among the first [`LIMIT`] candidates; where it is not among
    /// them, how many there are.
    fn rank(&self, pos: usize, len: usize, dist: usize) -> usize {
        let mut count = 0;
        let mut found = false;
        self.walk3(pos, len, |q| {
            found = pos - q == dist;
            count += !found as usize;
            !found
        });
        count
    }

    /// The distance of the match of rank `r` (as [`Mf::rank`]).
    fn nth(&self, pos: usize, len: usize, r: usize) -> Option<usize> {
        let (mut k, mut out) = (0, None);
        self.walk3(pos, len, |q| {
            if k == r {
                out = Some(pos - q);
            }
            k += 1;
            out.is_none()
        });
        out
    }

    /// Calls `f` with each position matching at least `len` bytes at `pos` on the chain of 3
    /// bytes, nearest first, among its first [`LIMIT`] within reach, while it returns true.
    fn walk3(&self, pos: usize, len: usize, mut f: impl FnMut(usize) -> bool) {
        let d = self.d;
        if pos + 3 > d.len() {
            return;
        }
        let mask = self.mask;
        let mut q = self.head[key(d, pos, 3)];
        for _ in 0..LIMIT {
            if q == NONE || pos.saturating_sub(q as usize) > self.reach {
                return;
            }
            let p = q as usize;
            // (the next position looked at ahead may be on the chains)
            if p < pos && d[p + len - 1] == d[pos + len - 1] && d[p..p + len] == d[pos..pos + len] && !f(p) {
                return;
            }
            q = self.prev[p & mask];
        }
    }
}

/// Bytes the headers' work of a block takes at most, of the Huffman codes, the tables written and
/// read, the codes made of them, and what a header read as it is takes: with room for the
/// vectors that grow (each size they pass) and for the holes that leaves.
pub const SCRATCH: usize = 256 << 10;

/// Candidates looked at on the chain of 3 bytes for an escape's rank.
pub const LIMIT: usize = 4096;

/// The contexts of a stream's record.
struct Ctx {
    /// Whether a match starts: by the token before, the longest pair's length and distance, and
    /// whether the next position has a longer match.
    is_match: Vec<Bit>,
    /// Whether the match is the tree's: by how it stands to the longest pair.
    bst: Vec<Bit>,
    /// The pair, from the longest: by its index and what stepping back gives up.
    pair: Vec<Bit>,
    /// Whether the length is all the pair's, else how much shorter.
    full: Vec<Bit>,
    shorter: Vec<Bit>,
    /// Escapes: the length, by the longest pair's; the rank among the matches of that length.
    escape: Vec<Bit>,
    rank: Vec<Bit>,
    /// The blocks: their sizes as cut (by whether a cut was seen), their headers as predicted.
    cut: Vec<Bit>,
    predicted: Vec<Bit>,
    /// The reach, the bits of headers as they are, distances past the ranks; the tail's length.
    misc: Vec<Bit>,
}

/// The lengths of the vectors of [`Ctx`], in the order of its fields.
pub const CTX_LENS: [usize; 10] = [2 * 12 * 7 * 3, 3 * 2, 3 * 4 * 4 * 2, 12, 12 * 32, 12 * 16, 12 * 24, 2, 1, 96];

impl Ctx {
    fn new() -> Self {
        let v = |i: usize| vec![Bit::default(); CTX_LENS[i]];
        Ctx {
            is_match: v(0),
            bst: v(1),
            pair: v(2),
            full: v(3),
            shorter: v(4),
            escape: v(5),
            rank: v(6),
            cut: v(7),
            predicted: v(8),
            misc: v(9),
        }
    }
}

/// What the record codes, for the statistics.
#[derive(Clone, Copy)]
#[repr(usize)]
pub enum Cat {
    /// Whether a match starts.
    Start,
    /// Which pair.
    Pair,
    /// A length shorter than the pair's.
    Len,
    /// Matches not among the pairs.
    Escape,
    /// The blocks' sizes and headers, the end.
    Blocks,
}
pub const CATS: usize = 5;

/// What codes the record's bits ([`Coder`]: the arithmetic encoder, which writes the value
/// given, or decoder, which returns what it reads), of this crate's [`Cat`]s and errors.
trait Bits: Coder {
    /// What the bits after are, for the statistics.
    #[inline]
    fn cat(&mut self, c: Cat) {
        self.set_cat(c as usize)
    }

    /// [`Coder::gamma32`], failing as corrupt input.
    fn gamma(&mut self, t: &mut [Bit], v: u32) -> Result<u32> {
        self.gamma32(t, v, corrupt)
    }
}

impl<C: Coder> Bits for C {}

/// The encoder, counting the bits of each [`Cat`].
type Enc = Costed<CATS>;

/// The walk over the data coding its tokens (`tokens` given to the encoder; the decoder's are
/// pushed onto it), the matches within `reach`.
fn walk(
    d: &[u8],
    reach: usize,
    bits: &mut impl Bits,
    ctx: &mut Ctx,
    tokens: &mut Vec<Token>,
    encoding: bool,
) -> Result<()> {
    let n = d.len();
    let mut mf = Mf::new(d, reach);
    let mut bst = Bst::new(d, reach);
    let mut pairs = Vec::with_capacity(PAIRS);
    let mut next_pairs = Vec::with_capacity(PAIRS);
    let (mut pos, mut prev, mut k) = (0, 0, 0);
    // (the position whose pairs `next_pairs` holds, found for the lazy context)
    let mut ahead = usize::MAX;
    while pos < n {
        mf.advance(pos);
        let cap = (n - pos).min(MAX_MATCH);
        if ahead == pos {
            std::mem::swap(&mut pairs, &mut next_pairs);
        } else {
            mf.pairs(pos, cap, &mut pairs);
        }
        let given = if encoding { *tokens.get(k).ok_or_else(corrupt)? } else { Token::Literal(0) };
        k += 1;
        let longest = pairs.last().copied().unwrap_or((0, 0));
        // (lazy matching: a longer match at the next position)
        let lazy = if longest.0 >= MIN_MATCH && pos + 1 < n {
            mf.advance(pos + 1);
            mf.pairs(pos + 1, (n - pos - 1).min(MAX_MATCH), &mut next_pairs);
            ahead = pos + 1;
            let nx = next_pairs.last().map_or(0, |p| p.0);
            (nx > longest.0) as usize + (nx > longest.0 + 1) as usize
        } else {
            0
        };
        let dc = if longest.0 == 0 { 6 } else { db(longest.1) };
        let c = ((prev * 12 + lb(longest.0)) * 7 + dc) * 3 + lazy;
        bits.cat(Cat::Start);
        if !bits.bit(&mut ctx.is_match[c], matches!(given, Token::Match { .. })) {
            tokens_push(tokens, encoding, Token::Literal(d[pos]));
            if encoding && given != Token::Literal(d[pos]) {
                return Err(Error("a literal not of the data"));
            }
            pos += 1;
            prev = 0;
            continue;
        }
        let (g_dist, g_len) = match given {
            Token::Match { len, dist } => (dist, len as u32),
            _ => (0, 0),
        };
        // the tree's match
        bst.advance(pos);
        let (bl, bd) = bst.last;
        let bl = bl.min(cap);
        if bl >= MIN_MATCH {
            let rel = if (bl, bd) == longest {
                0
            } else if bl == longest.0 {
                1
            } else {
                2
            };
            bits.cat(Cat::Pair);
            if bits.bit(&mut ctx.bst[rel * 2 + prev], (g_len as usize, g_dist as usize) == (bl, bd)) {
                let t = Token::Match { len: bl as u16, dist: bd as u32 };
                if bd > reach || bd > pos {
                    return Err(corrupt());
                }
                tokens_push(tokens, encoding, t);
                prev = 1;
                pos += bl;
                if bits.failed() {
                    return Err(corrupt());
                }
                continue;
            }
        }
        // the pair from the longest: j steps back; past them all, an escape
        let m = pairs.len();
        let gj = pairs.iter().rposition(|&(l, dist)| dist as u32 == g_dist && l as u32 >= g_len);
        let gj = gj.map_or(m, |i| m - 1 - i);
        let mut j = 0;
        bits.cat(Cat::Pair);
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
        let (dist, len) = if j < m {
            let (top, dist) = pairs[m - 1 - j];
            bits.cat(Cat::Len);
            let len = if bits.bit(&mut ctx.full[lb(top)], g_len == top as u32) {
                top as u32
            } else {
                let s = bits
                    .gamma(&mut ctx.shorter[lb(top) * 32..lb(top) * 32 + 32], (top as u32).wrapping_sub(g_len + 1))?;
                (top as u32).checked_sub(s + 1).ok_or_else(corrupt)?
            };
            (dist as u32, len)
        } else {
            bits.cat(Cat::Escape);
            // the length, then the rank among the matches of that length, nearest first; past those
            // looked at, the distance
            let lc = lb(longest.0);
            let len = bits.gamma(&mut ctx.escape[lc * 16..lc * 16 + 16], g_len.wrapping_sub(MIN_MATCH as u32))?;
            let len = len.checked_add(MIN_MATCH as u32).filter(|&l| l as usize <= cap).ok_or_else(corrupt)?;
            let rank = if encoding { mf.rank(pos, len as usize, g_dist as usize) } else { 0 };
            let lc = lb(len as usize);
            let r = bits.gamma(&mut ctx.rank[lc * 24..lc * 24 + 24], rank as u32)? as usize;
            let dist = match mf.nth(pos, len as usize, r) {
                Some(dist) => dist as u32,
                None => bits.gamma(&mut ctx.misc[..32], g_dist.wrapping_sub(1))?.wrapping_add(1),
            };
            (dist, len)
        };
        let (du, lu) = (dist as usize, len as usize);
        if du == 0 || du > pos || du > reach || lu < MIN_MATCH || lu > cap || lcp(d, pos - du, pos) < lu {
            return Err(Error("a match that does not fit"));
        }
        let t = Token::Match { len: len as u16, dist };
        if encoding && t != given {
            return Err(corrupt());
        }
        tokens_push(tokens, encoding, t);
        prev = 1;
        pos += lu;
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

/// The cut of blocks: a compressor buffering its tokens ends a block after a multiple of 8 tokens
/// once their bytes (a literal 1, a match 3, a flag byte for each 8) reach a threshold, when
/// another token follows. What the blocks before tell of the threshold.
struct Cut {
    lo: u64,
    hi: u64,
    known: bool,
    broken: bool,
}

impl Cut {
    fn new() -> Cut {
        Cut { lo: 0, hi: u64::MAX, known: false, broken: false }
    }

    /// The bytes of the first `k` tokens of `t`, with their flag bytes.
    fn bytes(t: &[Token], k: usize) -> u64 {
        let m = t[..k].iter().filter(|t| matches!(t, Token::Match { .. })).count();
        (k + 2 * m + k.div_ceil(8)) as u64
    }

    /// The size predicted for the block of the tokens `t` left.
    fn predict(&self, t: &[Token]) -> usize {
        let rest = t.len().min(u16::MAX as usize);
        if !self.known || self.broken {
            return rest;
        }
        let mut b = 0u64;
        for (i, tok) in t.iter().enumerate().take(rest) {
            if i % 8 == 0 {
                if i > 0 && b >= self.lo {
                    return i;
                }
                b += 1;
            }
            b += if matches!(tok, Token::Match { .. }) { 3 } else { 1 };
        }
        rest
    }

    /// Learns from a block of `size` tokens of the tokens `t` left.
    fn learn(&mut self, t: &[Token], size: usize) {
        if size >= t.len() {
            return;
        }
        if !size.is_multiple_of(8) || size == 0 {
            self.broken = true;
            return;
        }
        // (its bytes reached the threshold at its end, and not 8 tokens before)
        let lo = if size >= 8 { Self::bytes(t, size - 8) + 1 } else { 0 };
        self.lo = self.lo.max(lo);
        self.hi = self.hi.min(Self::bytes(t, size));
        self.known = true;
        if self.lo > self.hi {
            self.broken = true;
        }
    }
}

/// The bits of a header written as it is: their count and the bytes.
fn head_bits(h: &Head, m: Method) -> Result<(u32, Vec<u8>)> {
    let k = lzh::codes(h, m)?;
    let mut w = lzh::Writer::default();
    lzh::write_head(&mut w, h, m, &k)?;
    let n = (w.out.len() * 8) as u32 + w.pending();
    let pad = (8 - w.pending() % 8) % 8;
    w.put(pad, 0);
    Ok((n, w.out))
}

/// The blocks, the padding and the tail of `st` (its tokens known), coded: each block's size as
/// cut or as it is, its header as predicted or as it is.
///
/// The stream is written onto the end of `out` (which the decoder reserves room in, so that it
/// does not grow). The headers' work (the Huffman codes of a block, the tables, the codes read)
/// takes memory of sizes that the tokens decide, all of it within [`SCRATCH`] bytes: the decoder
/// takes them from the stack once before, so that what the module takes does not depend on them.
fn code_blocks(
    bits: &mut impl Bits,
    ctx: &mut Ctx,
    st: &mut Stream,
    m: Method,
    encoding: bool,
    out: Vec<u8>,
) -> Result<Vec<u8>> {
    bits.cat(Cat::Blocks);
    if !encoding {
        std::hint::black_box(Vec::<u8>::with_capacity(SCRATCH));
    }
    let mut w = lzh::Writer::onto(out);
    let mut cut = Cut::new();
    let mut at = 0;
    let mut b = 0;
    let tokens = std::mem::take(&mut st.tokens);
    while at < tokens.len() {
        let left = &tokens[at..];
        let guess = cut.predict(left);
        let given = if encoding { st.heads.get(b).ok_or_else(corrupt)?.size as usize } else { 0 };
        let size = if bits.bit(&mut ctx.cut[cut.known as usize], given == guess) {
            guess
        } else {
            bits.raws(16, given as u64) as usize
        };
        if size == 0 || size > left.len() {
            return Err(corrupt());
        }
        cut.learn(left, size);
        let toks = &left[..size];
        let pred = huff::head(toks, m);
        let given = if encoding { st.heads[b].clone() } else { Head::default() };
        let h = if bits.bit(&mut ctx.predicted[0], given == pred) {
            pred
        } else {
            let (n, bytes) = if encoding { head_bits(&given, m)? } else { (0, Vec::new()) };
            let n = bits.gamma(&mut ctx.misc[..32], n)?;
            if n > 1 << 16 {
                return Err(corrupt());
            }
            let mut out = Vec::new();
            for i in 0..n.div_ceil(8) {
                let k = (n - 8 * i).min(8);
                let v = bytes.get(i as usize).map_or(0, |&x| (x >> (8 - k)) as u32);
                out.push((bits.raws(k, v as u64) << (8 - k)) as u8);
            }
            let mut r = lzh::Reader::new(&out);
            let (h, _) = lzh::read_head(&mut r, m)?;
            if r.pos() != n as usize || h.size as usize != size {
                return Err(corrupt());
            }
            h
        };
        let k = lzh::codes(&h, m)?;
        lzh::write_block(&mut w, &h, &k, toks, m)?;
        at += size;
        b += 1;
        if bits.failed() {
            return Err(corrupt());
        }
    }
    if encoding && b != st.heads.len() {
        return Err(corrupt());
    }
    // (the heads of the encoder's stream are known; the decoder keeps none)
    st.tokens = tokens;
    // the padding, and the bytes after it
    st.pad_bits = ((8 - w.pending() % 8) % 8) as u8;
    st.pad = bits.raws(st.pad_bits as u32, st.pad as u64) as u8;
    let n = bits.gamma(&mut ctx.misc[32..64], st.tail.len() as u32)? as usize;
    if !encoding {
        if n > 1 << 24 {
            return Err(corrupt());
        }
        // (the tail is written as it is read)
        lzh::pad_onto(&mut w, st.pad, st.pad_bits)?;
        for _ in 0..n {
            w.out.push(bits.raws(8, 0) as u8);
        }
        if bits.failed() {
            return Err(corrupt());
        }
        return Ok(w.out);
    }
    for &x in &st.tail {
        bits.raws(8, x as u64);
    }
    if bits.failed() {
        return Err(corrupt());
    }
    lzh::finish(w, st.pad, st.pad_bits, &st.tail)
}

/// A stream split: its data and record.
pub struct Split {
    pub data: Vec<u8>,
    pub record: Vec<u8>,
    /// Bits of the record by [`Cat`].
    pub cost: [f64; CATS],
}

/// Splits the stream `input` of method `m` and `size` bytes of data. Fails where it does not
/// decode, or is not what encoding its parse gives.
pub fn split(input: &[u8], size: usize, m: Method) -> Result<Split> {
    let mut st = lzh::decode(input, size, m)?;
    let (record, cost) = record(input, &mut st, m)?;
    Ok(Split { data: st.data, record, cost })
}

/// [`split`]'s record of `st`, the stream `input`.
fn record(input: &[u8], st: &mut Stream, m: Method) -> Result<(Vec<u8>, [f64; CATS])> {
    let reach = st.tokens.iter().map(|t| if let Token::Match { dist, .. } = t { *dist as usize } else { 0 }).max();
    let reach = reach.unwrap_or(0);
    let mut e = Enc::new(Cat::Blocks as usize);
    let mut ctx = Ctx::new();
    e.gamma(&mut ctx.misc[..32], reach as u32)?;
    e.gamma(&mut ctx.misc[64..], st.tokens.len() as u32)?;
    let mut tokens = std::mem::take(&mut st.tokens);
    walk(&st.data, reach, &mut e, &mut ctx, &mut tokens, true)?;
    st.tokens = tokens;
    let out = code_blocks(&mut e, &mut ctx, st, m, true, Vec::new())?;
    if out != input {
        return Err(Error("a stream encoded otherwise"));
    }
    let mut record = vec![m.number()];
    record.extend(e.c.finish());
    Ok((record, e.cost))
}

/// What a record starts with: the stream's method, its reach and its count of tokens.
pub struct Start {
    pub method: Method,
    pub reach: usize,
    pub tokens: usize,
}

/// The start of `record` for a stream of `n` bytes of data, with the decoder and the contexts
/// as they are after it.
fn start<'a>(record: &'a [u8], n: usize, ctx: &mut Ctx) -> Result<(Start, ArithDecoder<'a>)> {
    let (&mn, rest) = record.split_first().ok_or_else(corrupt)?;
    let method = Method::of_number(mn).ok_or_else(corrupt)?;
    let mut dec = ArithDecoder::new(rest);
    let reach = dec.gamma(&mut ctx.misc[..32], 0)? as usize;
    if reach > method.window() {
        return Err(corrupt());
    }
    // (a token covers a byte at least)
    let tokens = dec.gamma(&mut ctx.misc[64..], 0)? as usize;
    if tokens > n {
        return Err(corrupt());
    }
    Ok((Start { method, reach, tokens }, dec))
}

/// The start of `record` for a stream of `n` bytes of data: what the memory plan follows
/// ([`crate::plan`]).
pub fn start_of(record: &[u8], n: usize) -> Result<Start> {
    Ok(start(record, n, &mut Ctx::new())?.0)
}

/// The stream that `data` and `record` rebuild, written onto `out`.
pub fn join_onto(data: &[u8], record: &[u8], out: &mut Vec<u8>) -> Result<()> {
    let mut ctx = Ctx::new();
    let (Start { method: m, reach, tokens: count }, mut dec) = start(record, data.len(), &mut ctx)?;
    // (the tokens reserved: their count is the record's)
    let mut tokens = Vec::with_capacity(count);
    walk(data, reach, &mut dec, &mut ctx, &mut tokens, false)?;
    if tokens.len() != count {
        return Err(Error("a count of tokens that does not fit"));
    }
    let mut st = Stream { tokens, ..Stream::default() };
    *out = code_blocks(&mut dec, &mut ctx, &mut st, m, false, std::mem::take(out))?;
    Ok(())
}

/// [`join_onto`] onto a new vector.
pub fn join(data: &[u8], record: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    join_onto(data, record, &mut out)?;
    Ok(out)
}
