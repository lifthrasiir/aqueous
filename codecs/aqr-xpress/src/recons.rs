//! The record of a stream of XPRESS Huffman chunks given its data (`FORMAT.md` §3), and the
//! stream rebuilt from both.
//!
//! A stream is chunks, each [`xpress`] blocks (WOF's chunks of 4, 8 or 16 KiB of data; a
//! buffer is a chunk of its own, of blocks of 64 KiB): the data is the chunks' one after another.
//! A chunk is stored (its data as it is: it is as long as its data) or compressed. The record
//! codes the chunk size, and for each chunk whether it is stored and, for a compressed one, its
//! parse given the data:
//!
//! - the tokens, under contexts of what a match finder sees in the data: the longest match
//!   there and whether one a byte on is longer; a match's length against the longest, an offset
//!   by its rank among those giving that length, nearest first. The blocks need no sizes: one
//!   ends at the token that brings it to 64 KiB;
//! - the code lengths after the tokens, each against the length a Huffman code of the block's
//!   frequencies gives it (the end symbol counted in the last block's, where there is one);
//! - the rest as it is: whether the end symbol follows the last block.
//!
//! Both sides walk the chunk alike, so the contexts are the same; the record's coder is
//! aqr-common's binary arithmetic coder, its contexts going on from chunk to chunk.

use aqr_common::arith::{ArithDecoder, ArithEncoder, Bit};
use aqr_common::lz::lcp_up;

use crate::xpress::{self, BLOCK, END, MAX_BITS, MAX_OFF, MIN_MATCH, Parse, SYMS, Tok};
use crate::{Error, Result, corrupt};

/// Candidates looked at for the longest match.
pub const DEPTH: usize = 16;
/// Candidates looked at for an offset's rank; past them, the offset is written as it is.
pub const LIMIT: usize = 4096;

const NONE: u32 = u32::MAX;
const WINDOW: usize = 1 << 16;

#[inline]
fn h3(d: &[u8], p: usize) -> usize {
    let k = d[p] as u32 | (d[p + 1] as u32) << 8 | (d[p + 2] as u32) << 16;
    (k.wrapping_mul(0x9e37_79b1) >> 16) as usize
}

type Heads = [u32; WINDOW];
/// Bytes of the match finder's heads, and of its links.
pub const MF_BYTES: u64 = 4 * WINDOW as u64;

/// The match finder of a stream: every position before the current one on a chain of a hash of
/// its first 3 bytes (their links in a ring of the window's size). Positions are those of the
/// stream's data, so that its chunks follow one another; a chunk's matches stay within it.
struct Mf {
    head: Box<Heads>,
    prev: Vec<u32>,
}

impl Mf {
    fn new() -> Mf {
        let head = vec![NONE; WINDOW].into_boxed_slice().try_into().expect("heads of 2^16");
        Mf { head, prev: vec![NONE; WINDOW] }
    }

    /// Starts a stream.
    fn reset(&mut self) {
        self.head.fill(NONE);
    }

    /// Inserts position `p`.
    #[inline]
    fn insert(&mut self, d: &[u8], p: usize) {
        if p + 3 <= d.len() {
            let h = h3(d, p);
            self.prev[p & (WINDOW - 1)] = self.head[h];
            self.head[h] = p as u32;
        }
    }

    /// The first candidate of the chain at `pos` (a match of 3 bytes or more fits there).
    #[inline]
    fn chain(&self, d: &[u8], pos: usize) -> u32 {
        let c = self.head[h3(d, pos)];
        // (`pos` itself may be on it, where the next position was searched)
        if c != NONE && c as usize >= pos { self.prev[c as usize & (WINDOW - 1)] } else { c }
    }

    /// The longest match at `pos` of at most `cap` bytes (3 or more), reaching back to `lo`,
    /// among the first [`DEPTH`] candidates: its length (0 where none) and offset, and the length
    /// of the nearest (0 where none).
    fn longest(&self, d: &[u8], pos: usize, lo: usize, cap: usize) -> (usize, usize, usize, usize) {
        let (mut best, mut off, mut first, mut first_off) = (0, 0, 0, 0);
        if cap < MIN_MATCH {
            return (0, 0, 0, 0);
        }
        let mut c = self.chain(d, pos);
        for i in 0..DEPTH {
            if c == NONE || (c as usize) < lo || best >= cap {
                break;
            }
            let p = c as usize;
            if i == 0 || ends_alike(d, p, pos, best.min(cap - 1)) {
                let n = lcp_up(d, p, pos, cap);
                if i == 0 {
                    (first, first_off) = (n, pos - p);
                }
                if n > best {
                    (best, off) = (n, pos - p);
                }
            }
            c = self.prev[p & (WINDOW - 1)];
        }
        if best >= MIN_MATCH { (best, off, first, first_off) } else { (0, 0, 0, 0) }
    }

    /// The record-setting candidates at `pos` (matches of at most `cap` bytes, 3 or more): those
    /// longer than every one nearer, nearest first, up to [`LIMIT`] candidates of the chain.
    /// Calls `f` with the offset and length of each, stopping where it returns true.
    fn setters(&self, d: &[u8], pos: usize, lo: usize, cap: usize, mut f: impl FnMut(usize, usize) -> bool) {
        let mut best = MIN_MATCH - 1;
        let mut c = self.chain(d, pos);
        for _ in 0..LIMIT {
            if c == NONE || (c as usize) < lo || best >= cap {
                return;
            }
            let p = c as usize;
            if ends_alike(d, p, pos, best) {
                let n = lcp_up(d, p, pos, cap);
                if n > best {
                    best = n;
                    if f(pos - p, n) {
                        return;
                    }
                }
            }
            c = self.prev[p & (WINDOW - 1)];
        }
    }

    /// The offsets of matches of at least `len` bytes (3 or more) at `pos`, nearest first, up
    /// to [`LIMIT`] candidates of the chain: calls `f` with each, stopping where it returns true.
    fn each(&self, d: &[u8], pos: usize, lo: usize, len: usize, mut f: impl FnMut(usize) -> bool) {
        let mut c = self.chain(d, pos);
        for _ in 0..LIMIT {
            if c == NONE || (c as usize) < lo {
                return;
            }
            let p = c as usize;
            if ends_alike(d, p, pos, len - 1) && lcp_up(d, p, pos, len) >= len && f(pos - p) {
                return;
            }
            c = self.prev[p & (WINDOW - 1)];
        }
    }
}

/// Whether `d[a..]` and `d[b..]`, `a < b`, may have more than `k` bytes in common: the bytes up
/// to `d[b + k]` (which is in `d`) as far as a word reaches back alike.
#[inline(always)]
fn ends_alike(d: &[u8], a: usize, b: usize, k: usize) -> bool {
    match k.checked_sub(3) {
        Some(s) => {
            let w = |p: usize| u32::from_le_bytes(d[p + s..p + s + 4].try_into().unwrap());
            w(a) == w(b)
        }
        None => d[a + k] == d[b + k],
    }
}

/// Bucket of a length: 0 for none, then 3, 4-5, 6-8, 9-16, 17-32, 33-128, more.
#[inline]
fn lb(n: usize) -> usize {
    match n {
        0..=2 => 0,
        3 => 1,
        4..=5 => 2,
        6..=8 => 3,
        9..=16 => 4,
        17..=32 => 5,
        33..=128 => 6,
        _ => 7,
    }
}

/// What the record codes, for the statistics.
#[derive(Clone, Copy)]
#[repr(usize)]
pub enum Cat {
    Kind,
    Len,
    Off,
    Table,
    Rest,
}
pub const CATS: usize = 5;

struct Ctx {
    stored: Bit,
    end: Bit,
    /// A token: a match or not (by the longest match, the next position's relation, how far the
    /// longest is, and the last token's kind).
    is_match: Vec<Bit>,
    /// The candidate a match is at, nearest first (by the first one's length and the longest's).
    cand: Vec<[Bit; 32]>,
    /// Whether the match is a record-setting candidate (longer than every one nearer), and which
    /// of them, nearest first.
    is_setter: Vec<Bit>,
    setter: Vec<[Bit; 32]>,
    /// Whether the offset is none of the candidates, and a length shorter than the match there.
    off_escape: Bit,
    short: Bit,
    short_by: [Bit; 32],
    /// Code lengths: as predicted or not (by the prediction, the last one's agreement and the
    /// kind of symbol), else the value (by the prediction and the kind).
    tree_eq: Vec<Bit>,
    tree_val: Vec<[Bit; 32]>,
}

/// The vectors of `Ctx` in the order they are made: how many elements, of how many adaptive
/// bits each (a gamma's table has 32).
pub const CTX_VECS: [(usize, usize); 6] =
    [(8 * 17 * 3 * 2 * 2, 1), (8 * 8, 32), (8 * 8, 1), (8 * 8, 32), (16 * 2 * 2, 1), (16 * 2, 32)];

/// Bytes of what the module holds on WebAssembly (32-bit pointers), checked there by
/// assertions: a stream's data and record and kept bytes, an adaptive bit, a token.
pub mod wasm {
    pub const STREAM: u64 = 24;
    pub const BIT: u64 = 4;
    pub const TOK: u64 = 8;
}

#[cfg(target_family = "wasm")]
const _: () = {
    assert!(size_of::<(&[u8], &[u8], &[u8])>() as u64 == wasm::STREAM);
    assert!(size_of::<Bit>() as u64 == wasm::BIT);
    assert!(size_of::<Tok>() as u64 == wasm::TOK);
};

/// Bytes past the output's length that the module reserves.
pub const SLACK: usize = 64;

/// Bytes the table's work of a block takes at most (Huffman's lists): with room for the vectors
/// that grow, and for the holes that leaves. The decoder takes them from the stack once before
/// the blocks and gives them back, so that what it takes does not depend on the data.
pub const SCRATCH: usize = 64 << 10;

impl Ctx {
    fn new() -> Ctx {
        let v = |i: usize| vec![Bit::default(); CTX_VECS[i].0];
        let g = |i: usize| vec![[Bit::default(); 32]; CTX_VECS[i].0];
        Ctx {
            stored: Bit::default(),
            end: Bit::default(),
            is_match: v(0),
            cand: g(1),
            is_setter: v(2),
            setter: g(3),
            off_escape: Bit::default(),
            short: Bit::default(),
            short_by: Default::default(),
            tree_eq: v(4),
            tree_val: g(5),
        }
    }
}

/// The side of the record's coder: the encoder, which knows each value, or the decoder, which
/// learns it.
trait Side {
    fn bit(&mut self, b: &mut Bit, v: bool, cat: Cat) -> bool;

    /// `n` bits of probability 1/2, from the top.
    fn raw(&mut self, n: u32, v: u32, cat: Cat) -> u32;

    /// `v < 2^31` as Elias gamma of `v + 1`: its bit length less 1 in unary under the adaptive
    /// bits `t` (the last one shared beyond), then the bits below the top one under `t`'s
    /// halves past 16.
    fn gamma(&mut self, t: &mut [Bit; 32], v: u32, cat: Cat) -> Result<u32> {
        let x = v.wrapping_add(1);
        let len = u32::BITS - x.leading_zeros();
        let mut b = 0;
        while self.bit(&mut t[b.min(15)], b + 1 < len as usize, cat) {
            b += 1;
            if b == 31 {
                return Err(corrupt());
            }
        }
        let mut low = 0;
        for i in (0..b).rev() {
            let k = 16 + b.min(15);
            low = low << 1 | self.bit(&mut t[if i + 1 == b { k } else { 31 }], x >> i & 1 != 0, cat) as u32;
        }
        Ok((1 << b | low) - 1)
    }

    /// A value of 5 bits by a tree of adaptive bits.
    fn tree5(&mut self, t: &mut [Bit; 32], v: u32, cat: Cat) -> u32 {
        let mut node = 1;
        for i in (0..5).rev() {
            node = node << 1 | self.bit(&mut t[node], v >> i & 1 != 0, cat) as usize;
        }
        (node - 32) as u32
    }
}

/// The encoder, counting the bits of each [`Cat`].
type Enc = aqr_common::arith::Costed<CATS>;

/// Each side of `aqr_common::arith`'s coder codes the record, each bit of a [`Cat`].
impl<C: aqr_common::arith::Coder> Side for C {
    #[inline]
    fn bit(&mut self, b: &mut Bit, v: bool, cat: Cat) -> bool {
        aqr_common::arith::Coder::set_cat(self, cat as usize);
        aqr_common::arith::Coder::bit(self, b, v)
    }

    #[inline]
    fn raw(&mut self, n: u32, v: u32, cat: Cat) -> u32 {
        aqr_common::arith::Coder::set_cat(self, cat as usize);
        aqr_common::arith::Coder::raws(self, n, v as u64) as u32
    }
}

/// Code lengths of a Huffman code of `freqs`, at most `limit` bits: the two least frequent
/// merged first (the lower symbol or the earlier merge first on ties), lengths past the limit
/// cut and the code mended by lengthening the longest codes below it. Symbols of no frequency
/// get none.
pub fn huffman(freqs: &[u32], limit: u8, out: &mut [u8]) {
    out.fill(0);
    let used: Vec<usize> = (0..freqs.len()).filter(|&s| freqs[s] > 0).collect();
    match used.len() {
        0 => return,
        1 => {
            out[used[0]] = 1;
            return;
        }
        _ => {}
    }
    // nodes: leaves then merges; each with its parent
    let n = used.len();
    let mut weight: Vec<u64> = used.iter().map(|&s| freqs[s] as u64).collect();
    let mut parent = vec![usize::MAX; 2 * n - 1];
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by_key(|&i| (weight[i], i));
    // two queues: leaves by weight, merges in the order made (their weights do not decrease)
    let (mut li, mut merges, mut mi) = (0, Vec::with_capacity(n - 1), 0);
    for _ in 0..n - 1 {
        let mut take = |weight: &Vec<u64>, merges: &Vec<usize>| {
            let leaf = order.get(li).map(|&i| weight[i]);
            let merge = merges.get(mi).map(|&i: &usize| weight[i]);
            match (leaf, merge) {
                (Some(l), Some(m)) if l <= m => {
                    li += 1;
                    order[li - 1]
                }
                (Some(_), None) => {
                    li += 1;
                    order[li - 1]
                }
                _ => {
                    mi += 1;
                    merges[mi - 1]
                }
            }
        };
        let a = take(&weight, &merges);
        let b = take(&weight, &merges);
        let node = weight.len();
        weight.push(weight[a] + weight[b]);
        parent[a] = node;
        parent[b] = node;
        merges.push(node);
    }
    let mut depth = vec![0u32; 2 * n - 1];
    for i in (0..2 * n - 2).rev() {
        depth[i] = depth[parent[i]] + 1;
    }
    let mut lens: Vec<u32> = depth[..n].to_vec();
    // past the limit: cut, then lengthen the longest codes below it until the code fits
    if lens.iter().any(|&l| l > limit as u32) {
        lens.iter_mut().for_each(|l| *l = (*l).min(limit as u32));
        let kraft = |lens: &[u32]| lens.iter().map(|&l| 1u64 << (limit as u32 - l)).sum::<u64>();
        let mut by: Vec<usize> = (0..n).collect();
        by.sort_by_key(|&i| (std::cmp::Reverse(lens[i]), weight[i], i));
        while kraft(&lens) > 1 << limit {
            let i = *by.iter().find(|&&i| lens[i] < limit as u32).expect("a code fits");
            lens[i] += 1;
            by.sort_by_key(|&i| (std::cmp::Reverse(lens[i]), weight[i], i));
        }
    }
    for (k, &s) in used.iter().enumerate() {
        out[s] = lens[k] as u8;
    }
}

/// What a walker keeps from stream to stream: the match finder (allocated once: streams are many,
/// and the allocator slow to find room where much is held; in a module, once for all its streams).
pub struct Spare {
    mf: Box<Mf>,
}

impl Spare {
    pub fn new() -> Spare {
        Spare { mf: Box::new(Mf::new()) }
    }

    /// This thread's last spare.
    fn parked() -> Option<Spare> {
        SPARE.with(|s| s.borrow_mut().take())
    }

    /// Parks the spare for this thread's next stream (a module keeps its own: none there).
    fn park(self) {
        if cfg!(not(target_family = "wasm")) {
            SPARE.with(|s| *s.borrow_mut() = Some(self));
        }
    }
}

impl Default for Spare {
    fn default() -> Spare {
        Spare::new()
    }
}

thread_local! {
    static SPARE: std::cell::RefCell<Option<Spare>> = const { std::cell::RefCell::new(None) };
}

/// Walks a stream's chunks over `data`, coding them (`want` given, each its parse or None
/// where stored) or decoding them (written onto `out`, a block at a time), as the side is.
struct Walker {
    ctx: Ctx,
    spare: Spare,
    /// The tokens of the block.
    toks: Vec<Tok>,
    /// Whether they stay within their capacity (a decoder's).
    fixed: bool,
    /// Whether every position of a match is a candidate for the next (else the tokens' starts).
    all: bool,
    /// The most bytes of output (a decoder's).
    limit: usize,
}

impl Walker {
    /// A walker on `spare`; with `toks` the block's tokens stay within that many, else they grow.
    fn new(mut spare: Spare, toks: Option<usize>, limit: usize, all: bool) -> Walker {
        spare.mf.reset();
        Walker {
            ctx: Ctx::new(),
            spare,
            toks: Vec::with_capacity(toks.unwrap_or(0)),
            fixed: toks.is_some(),
            all,
            limit,
        }
    }

    /// A walker on this thread's spare (else a new one).
    fn parked(toks: Option<usize>, limit: usize, all: bool) -> Walker {
        Walker::new(Spare::parked().unwrap_or_default(), toks, limit, all)
    }

    fn into_spare(self) -> Spare {
        self.spare
    }

    fn park(self) {
        self.into_spare().park();
    }
}

impl Walker {
    /// One chunk `d[base..base + n]` of the data: its parse coded or decoded, the chunk written
    /// onto `out` a block at a time when decoding.
    fn chunk(
        &mut self,
        side: &mut impl Side,
        d: &[u8],
        base: usize,
        n: usize,
        want: Option<&Option<Parse>>,
        out: Option<&mut Vec<u8>>,
    ) -> Result<()> {
        let mut out = out;
        let stored = side.bit(&mut self.ctx.stored, matches!(want, Some(None)), Cat::Rest);
        if stored {
            if let Some(out) = out {
                out.extend_from_slice(&d[base..base + n]);
            }
            return Ok(());
        }
        let want = want.map(|w| w.as_ref().ok_or(Error("a stream stored"))).transpose()?;
        let same = Error("a parse the record does not give back");
        let end = base + n;
        let (mut at, mut t, mut nb) = (base, 0, 0);
        let mut eob = false;
        while at < end {
            let wb = want.map(|w| w.blocks.get(nb).ok_or_else(corrupt)).transpose()?;
            self.toks.clear();
            let from = at;
            at = self.tokens(side, d, base, end, at, want.map(|w| &w.toks[..]), &mut t)?;
            let last = at == end;
            if last {
                eob = side.bit(&mut self.ctx.end, want.is_some_and(|w| w.end), Cat::Rest);
            }
            // the code lengths, against the code of the block's frequencies
            let mut freqs = [0u32; SYMS];
            let mut p = from;
            for tok in &self.toks {
                match *tok {
                    Tok { len: 0, .. } => {
                        freqs[d[p] as usize] += 1;
                        p += 1;
                    }
                    Tok { len, off } => {
                        let s = 256 + (len as usize - MIN_MATCH).min(15) + 16 * off.ilog2() as usize;
                        freqs[s] += 1;
                        p += len as usize;
                    }
                }
            }
            freqs[END] += eob as u32;
            let mut pred = [0u8; SYMS];
            huffman(&freqs, MAX_BITS, &mut pred);
            let mut lens = Box::new([0u8; SYMS]);
            let mut agree = 1;
            for s in 0..SYMS {
                let v = wb.map_or(0, |b| b.lens[s]);
                lens[s] = self.length(side, pred[s], agree, s >= 256, v)?;
                agree = (lens[s] == pred[s]) as usize;
            }
            if let Some(o) = out.as_deref_mut() {
                xpress::write_block(o, &lens, &self.toks, d, from, last && eob, self.limit).ok_or_else(corrupt)?;
            }
            if let Some(pw) = want {
                let n = self.toks.len();
                if wb.is_some_and(|b| b.lens != lens || b.toks != n)
                    || pw.toks.get(t - n..t) != Some(&self.toks[..])
                    || (last && pw.end != eob)
                {
                    return Err(same);
                }
            }
            nb += 1;
        }
        if let Some(pw) = want
            && (nb != pw.blocks.len() || t != pw.toks.len())
        {
            return Err(same);
        }
        Ok(())
    }

    /// A code length, against `pred`.
    fn length(&mut self, side: &mut impl Side, pred: u8, agree: usize, matched: bool, v: u8) -> Result<u8> {
        let c = &mut self.ctx;
        let k = pred as usize * 2 + matched as usize;
        if side.bit(&mut c.tree_eq[k * 2 + agree], v == pred, Cat::Table) {
            return Ok(pred);
        }
        let x = side.tree5(&mut c.tree_val[k], v as u32, Cat::Table) as u8;
        if x > MAX_BITS || x == pred {
            return Err(corrupt());
        }
        Ok(x)
    }

    /// The tokens of a block from `pos` to the chunk's `end` (or 64 KiB on): coded from `want`
    /// (all its tokens, from the `t`-th) or decoded, onto `self.toks`; the position it ends at.
    #[allow(clippy::too_many_arguments)]
    fn tokens(
        &mut self,
        side: &mut impl Side,
        d: &[u8],
        start: usize,
        end: usize,
        mut pos: usize,
        want: Option<&[Tok]>,
        t: &mut usize,
    ) -> Result<usize> {
        let c = &mut self.ctx;
        let mf = &mut *self.spare.mf;
        let (toks, fixed, all) = (&mut self.toks, self.fixed, self.all);
        let stop = pos + BLOCK;
        let mut prev_match = 0;
        while pos < end && pos < stop {
            let w = want.map(|w| w.get(*t).copied().ok_or_else(corrupt)).transpose()?;
            *t += 1;
            if toks.len() == toks.capacity() && fixed {
                return Err(Error("more tokens than a block holds"));
            }
            let cap = end - pos;
            let lo = start.max(pos.saturating_sub(MAX_OFF));
            let (longest, _, first, off1) = mf.longest(d, pos, lo, cap);
            mf.insert(d, pos);
            // (how the next position's longest match compares: none, as long, a byte longer, more)
            let rel = if longest >= MIN_MATCH && cap > MIN_MATCH {
                let (next, ..) = mf.longest(d, pos + 1, start.max((pos + 1).saturating_sub(MAX_OFF)), cap - 1);
                if next <= longest {
                    0
                } else if next == longest + 1 {
                    1
                } else {
                    2
                }
            } else {
                0
            };
            let is_match = if cap < MIN_MATCH {
                if w.is_some_and(|t| t.len != 0) {
                    return Err(corrupt());
                }
                false
            } else {
                let ob = |o: usize| if o == 0 { 0 } else { o.ilog2() as usize + 1 };
                let ctx = (((lb(first) * 17 + ob(off1)) * 3 + rel) * 2 + prev_match) * 2 + (longest > first) as usize;
                side.bit(&mut c.is_match[ctx], w.is_some_and(|t| t.len != 0), Cat::Kind)
            };
            prev_match = is_match as usize;
            if !is_match {
                toks.push(Tok::LIT);
                pos += 1;
                continue;
            }
            let (wlen, woff) = w.map_or((0, 0), |t| (t.len as usize, t.off as usize));
            // the match: one of the record-setting candidates (longer than every one nearer), by
            // its rank among them, else an offset by its rank among the candidates, or as it is
            let fx = lb(first) * 8 + lb(longest);
            let (mut setter, mut found) = (0u32, false);
            if want.is_some() && woff != 0 && woff <= pos - lo && wlen == lcp_up(d, pos - woff, pos, cap) {
                mf.setters(d, pos, lo, cap, |o, _| {
                    found = o == woff;
                    setter += 1;
                    o >= woff
                });
                setter -= 1;
            }
            let (off, len);
            if side.bit(&mut c.is_setter[fx], found, Cat::Off) {
                let rank = side.gamma(&mut c.setter[fx], setter, Cat::Off)?;
                if want.is_some() {
                    (off, len) = (woff, wlen);
                } else {
                    let (mut n, mut at) = (0, (0, 0));
                    mf.setters(d, pos, lo, cap, |o, l| {
                        at = (o, l);
                        n += 1;
                        n > rank
                    });
                    if n <= rank {
                        return Err(corrupt());
                    }
                    (off, len) = at;
                }
            } else {
                let mut rank = 0u32;
                let mut found = false;
                if want.is_some() {
                    mf.each(d, pos, lo, MIN_MATCH, |o| {
                        if o == woff {
                            found = true;
                            return true;
                        }
                        rank += 1;
                        false
                    });
                }
                let esc = side.bit(&mut c.off_escape, want.is_some() && !found, Cat::Off);
                off = if esc {
                    side.raw(16, woff as u32, Cat::Off) as usize
                } else {
                    let rank = side.gamma(&mut c.cand[fx], rank, Cat::Off)?;
                    // (split: the offset of the rank is the one found for it)
                    if want.is_some() {
                        woff
                    } else {
                        let (mut n, mut off) = (0, 0);
                        mf.each(d, pos, lo, MIN_MATCH, |o| {
                            if n == rank {
                                off = o;
                                return true;
                            }
                            n += 1;
                            false
                        });
                        off
                    }
                };
                if off == 0 || off > pos - lo {
                    return Err(corrupt());
                }
                // the length: all the match at the offset has, or shorter
                let all = lcp_up(d, pos - off, pos, cap);
                len = if side.bit(&mut c.short, wlen < all, Cat::Len) {
                    let by = side.gamma(&mut c.short_by, all.saturating_sub(wlen + 1) as u32, Cat::Len)? as usize;
                    all.checked_sub(by + 1).ok_or_else(corrupt)?
                } else {
                    all
                };
            }
            if len < MIN_MATCH || off == 0 || off > pos - lo || len > cap || lcp_up(d, pos - off, pos, len) < len {
                return Err(corrupt());
            }
            toks.push(Tok { len: len as u32, off: off as u32 });
            if all {
                for q in pos + 1..pos + len {
                    mf.insert(d, q);
                }
            }
            pos += len;
        }
        Ok(pos)
    }
}

/// The data and record of a stream of chunks, each its bytes and its data's length (a chunk of
/// as many bytes as its data is stored).
pub struct Split {
    pub data: Vec<u8>,
    pub record: Vec<u8>,
    pub cost: [f64; CATS],
}

/// The chunk size that `chunks` have (all but the last, which has at most that many).
fn chunk_size(chunks: &[(&[u8], usize)]) -> Result<usize> {
    let size = chunks.first().ok_or(Error("no chunks"))?.1;
    for (i, c) in chunks.iter().enumerate() {
        if c.1 == 0 || c.1 > size || c.1 < size && i + 1 < chunks.len() {
            return Err(Error("chunks of another size"));
        }
    }
    Ok(size)
}

/// Splits a stream of chunks: None where a chunk does not read whole, or the chunks are not all
/// of one size (but the last, which is shorter).
pub fn split(chunks: &[(&[u8], usize)]) -> Result<Split> {
    if chunks.is_empty() {
        return Ok(Split { data: Vec::new(), record: Vec::new(), cost: [0.0; CATS] });
    }
    let mut data = vec![0; data_len(chunks)];
    let parses = read_into(chunks, &mut data)?;
    let size = chunk_size(chunks)?;
    let enc = best_walk(&data, size, &parses)?;
    Ok(Split { data, record: enc.c.finish(), cost: enc.cost })
}

/// Bytes of the data of a stream of chunks.
pub fn data_len(chunks: &[(&[u8], usize)]) -> usize {
    chunks.iter().map(|c| c.1).sum()
}

/// [`split`] with the data written into `data` (of [`data_len`] bytes), as where a container
/// holds it all: the record (its cost not counted).
pub fn split_into(chunks: &[(&[u8], usize)], data: &mut [u8]) -> Result<Vec<u8>> {
    if chunks.is_empty() {
        return Ok(Vec::new());
    }
    let parses = read_into(chunks, data)?;
    let size = chunk_size(chunks)?;
    Ok(best_walk(data, size, &parses)?.c.finish())
}

/// Bytes of data that the choice of [`walk`]'s candidates is made on.
const SAMPLE: usize = 256 << 10;

/// The record of `data` coded with the candidates the stream's encoder seems to use: tried on its
/// first chunks (up to [`SAMPLE`] bytes), the cheaper going on to the rest.
fn best_walk(data: &[u8], size: usize, parses: &[Option<Parse>]) -> Result<Enc> {
    let run = |parses: &[Option<Parse>], data: &[u8], all: bool| {
        let mut enc = Enc::with(ArithEncoder::new(Vec::new()));
        walk(&mut enc, data, size, parses, all).map(|_| enc)
    };
    let k = (SAMPLE / size).max(1).min(parses.len());
    let sample = &data[..(k * size).min(data.len())];
    let (a, b) = (run(&parses[..k], sample, false)?, run(&parses[..k], sample, true)?);
    let all = b.cost.iter().sum::<f64>() < a.cost.iter().sum::<f64>();
    if k == parses.len() {
        return Ok(if all { b } else { a });
    }
    run(parses, data, all)
}

/// The record of the chunks of `data` of `parses` coded onto `enc`.
fn walk(enc: &mut impl Side, data: &[u8], size: usize, parses: &[Option<Parse>], all: bool) -> Result<()> {
    let mut g = [Bit::default(); 32];
    enc.gamma(&mut g, size as u32, Cat::Rest)?;
    enc.bit(&mut Bit::default(), all, Cat::Rest);
    let mut w = Walker::parked(None, usize::MAX, all);
    let done = (|| {
        for (i, p) in parses.iter().enumerate() {
            w.chunk(enc, data, i * size, size.min(data.len() - i * size), Some(p), None)?;
        }
        Ok(())
    })();
    w.park();
    done
}

/// The data of a stream of chunks written into `data` (of [`data_len`] bytes), as [`split_into`]
/// writes it, without the record: the stream's data made again where it was not kept.
pub fn data_into(chunks: &[(&[u8], usize)], data: &mut [u8]) -> Result<()> {
    read_into(chunks, data).map(drop)
}

/// The data of a stream of chunks written into `data`, and the parse of each chunk (none for
/// one stored).
fn read_into(chunks: &[(&[u8], usize)], data: &mut [u8]) -> Result<Vec<Option<Parse>>> {
    if data.len() != data_len(chunks) {
        return Err(Error("data of another length"));
    }
    if data.len() >= u32::MAX as usize - WINDOW {
        return Err(Error("data too long"));
    }
    if !chunks.is_empty() {
        chunk_size(chunks)?;
    }
    let mut parses = Vec::with_capacity(chunks.len());
    let mut at = 0;
    for &(c, n) in chunks {
        let out = &mut data[at..at + n];
        at += n;
        if c.len() == n {
            out.copy_from_slice(c);
            parses.push(None);
            continue;
        }
        let (d, p) = xpress::read(c, n).ok_or(Error("a chunk not read"))?;
        out.copy_from_slice(&d);
        parses.push(Some(p));
    }
    Ok(parses)
}

/// The most tokens a block of chunks of `size` bytes of data holds.
pub fn block_toks(size: usize) -> usize {
    size.min(BLOCK)
}

/// The stream that `data` and `record` rebuild.
pub fn join(data: &[u8], record: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(data.len() / 2 + 64);
    join_onto(data, record, &mut out)?;
    Ok(out)
}

/// [`join`] onto `out`.
pub fn join_onto(data: &[u8], record: &[u8], out: &mut Vec<u8>) -> Result<()> {
    let (spare, done) = join_in(Spare::parked().unwrap_or_default(), data, record, out, usize::MAX);
    spare.park();
    done
}

/// The chunk size that `record` starts with, of a stream of `n` bytes of data, and whether the
/// candidates are all the positions.
pub fn read_head(dec: &mut ArithDecoder, n: usize) -> Result<(usize, bool)> {
    let mut g = [Bit::default(); 32];
    let size = dec.gamma(&mut g, 0, Cat::Rest)? as usize;
    if size == 0 || size > n {
        return Err(corrupt());
    }
    Ok((size, dec.bit(&mut Bit::default(), false, Cat::Rest)))
}

/// The stream that `data` and `record` rebuild, onto `out` (room for `limit` bytes, and a little
/// more, reserved), on `spare`: the spare back, to go on with.
pub fn join_in(spare: Spare, data: &[u8], record: &[u8], out: &mut Vec<u8>, limit: usize) -> (Spare, Result<()>) {
    if data.is_empty() {
        return (spare, if record.is_empty() { Ok(()) } else { Err(corrupt()) });
    }
    let mut dec = ArithDecoder::new(record);
    let (size, all) = match read_head(&mut dec, data.len()) {
        Ok(s) => s,
        Err(e) => return (spare, Err(e)),
    };
    let mut w = Walker::new(spare, Some(block_toks(size)), limit, all);
    // (the work of the blocks' tables takes memory of sizes the tokens decide: all of it within
    // SCRATCH bytes, taken from the stack once before the blocks)
    std::hint::black_box(Vec::<u8>::with_capacity(SCRATCH));
    let done = (|| {
        if data.len() >= u32::MAX as usize - WINDOW {
            return Err(corrupt());
        }
        for i in 0..data.len().div_ceil(size) {
            w.chunk(&mut dec, data, i * size, size.min(data.len() - i * size), None, Some(out))?;
            if dec.overrun() {
                return Err(corrupt());
            }
        }
        Ok(())
    })();
    (w.into_spare(), done)
}
