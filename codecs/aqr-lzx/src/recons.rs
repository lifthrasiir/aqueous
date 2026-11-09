//! The record of a stream of LZX chunks given its data (`FORMAT.md` §4), and the stream
//! rebuilt from both.
//!
//! A stream is the chunks of a WIM resource, one after another: the data is the resource's
//! (e8 undone), each chunk [`CHUNK`] bytes of it but the last. A chunk is stored (its data as
//! it is) or LZX ([`lzx`]). The record codes, for each chunk, whether it is stored and, for an
//! LZX chunk, its parse given the data:
//!
//! - the tokens of each block, under contexts of what a match finder sees in the data: the
//!   longest match there and the repeat offsets' matches; a match's length against those, a new
//!   offset by its rank among the offsets giving that length, nearest first;
//! - the trees after the tokens: each code length against the length a Huffman code of the
//!   block's frequencies gives it; how the pretree writes them (runs taken or not) under
//!   contexts of the runs there are; the pretree's lengths as the trees' are;
//! - the rest as it is, under adaptive bits: block types and sizes, uncompressed blocks'
//!   padding, repeat offsets and pad byte, the last word's padding.
//!
//! Both sides walk the chunk alike, so the contexts are the same; the record's coder is
//! aqr-common's binary arithmetic coder, its contexts going on from chunk to chunk.

use aqr_common::arith::{ArithDecoder, ArithEncoder, Bit};

use crate::lzx::{
    self, ALIGNED, ALIGNED_BLOCK, BitW, Block, LEN, MAIN_MAX, MAX_MATCH, MIN_MATCH, PRE, Parse, PreSym, Tok, TreeCode,
    UNCOMPRESSED, Variant, WINDOW,
};
use crate::{Error, Result, corrupt};

use aqr_common::lz::lcp_up;

/// Bytes of the data of a chunk.
pub const CHUNK: usize = WINDOW;
/// Candidates looked at for the longest match, on the chain of 3 bytes.
pub const DEPTH: usize = 32;
/// Candidates looked at for an offset's rank; past them, the offset is written as it is.
pub const LIMIT: usize = 4096;

const NONE: u32 = u32::MAX;

#[inline]
fn k2(d: &[u8], p: usize) -> u16 {
    d[p] as u16 | (d[p + 1] as u16) << 8
}

#[inline]
fn h3(d: &[u8], p: usize) -> u16 {
    hash3(d[p] as u32 | (d[p + 1] as u32) << 8 | (d[p + 2] as u32) << 16)
}

/// The hash of 3 bytes (little-endian, the top byte clear).
#[inline]
fn hash3(k: u32) -> u16 {
    (k.wrapping_mul(0x9e37_79b1) >> 16) as u16
}

/// The heads of chains, by a key of 16 bits (indexed so without bounds to check).
type Heads = [u32; 1 << 16];

fn heads() -> Box<Heads> {
    vec![NONE; 1 << 16].into_boxed_slice().try_into().expect("heads of 2^16")
}

/// The match finder of a stream: every position before the current one within the window, on
/// a chain of its first 2 bytes and one of a hash of its first 3 (their links in rings of the
/// window's size).
struct Mf {
    head2: Box<Heads>,
    prev2: Vec<u32>,
    head3: Box<Heads>,
    prev3: Vec<u32>,
    next: usize,
    /// The rings' mask, and the farthest offset a candidate may be.
    mask: usize,
    max_off: usize,
}

impl Mf {
    fn new(window: usize) -> Mf {
        Mf {
            head2: heads(),
            prev2: vec![NONE; window],
            head3: heads(),
            prev3: vec![NONE; window],
            next: 0,
            mask: window - 1,
            max_off: window,
        }
    }

    /// Starts a stream whose matches reach at most `max_off` back.
    fn reset(&mut self, max_off: usize) {
        self.head2.fill(NONE);
        self.head3.fill(NONE);
        self.next = 0;
        self.max_off = max_off;
    }

    /// Inserts the positions before `pos`.
    #[inline]
    fn advance(&mut self, d: &[u8], pos: usize) {
        let Mf { head2, prev2, head3, prev3, next, mask, .. } = self;
        let (mut p, mask) = (*next, *mask);
        let (prev2, prev3) = (&mut prev2[..=mask], &mut prev3[..=mask]);
        // (both keys of a word read once, where 4 bytes are left)
        let words = pos.min(d.len().saturating_sub(3));
        while p < words {
            let k = u32::from_le_bytes(d[p..p + 4].try_into().unwrap());
            let (h, k) = (hash3(k & 0xff_ffff) as usize, k as u16 as usize);
            prev3[p & mask] = head3[h];
            head3[h] = p as u32;
            prev2[p & mask] = head2[k];
            head2[k] = p as u32;
            p += 1;
        }
        while p < pos {
            if p + 3 <= d.len() {
                let h = h3(d, p) as usize;
                prev3[p & mask] = head3[h];
                head3[h] = p as u32;
            }
            if p + 2 <= d.len() {
                let k = k2(d, p) as usize;
                prev2[p & mask] = head2[k];
                head2[k] = p as u32;
            }
            p += 1;
        }
        *next = p;
    }

    /// Whether candidate `c` is one at `pos`: a position within reach.
    #[inline]
    fn near(&self, c: u32, pos: usize) -> bool {
        c != NONE && pos - c as usize <= self.max_off
    }

    /// The chain a match of `len` bytes at `pos` is on: its first candidate and its links.
    #[inline]
    fn chain(&self, d: &[u8], pos: usize, len: usize) -> (u32, &[u32]) {
        if len >= 3 && pos + 3 <= d.len() {
            (self.head3[h3(d, pos) as usize], &self.prev3)
        } else if pos + 2 <= d.len() {
            (self.head2[k2(d, pos) as usize], &self.prev2)
        } else {
            (NONE, &self.prev2)
        }
    }

    /// The longest match at `pos` of at most `cap` bytes, among the first [`DEPTH`] candidates
    /// of the chain of 3 and the nearest of 2: its length (0 where none) and offset.
    fn longest(&self, d: &[u8], pos: usize, cap: usize) -> (usize, usize) {
        let (mut best, mut off) = (0, 0);
        if cap < MIN_MATCH {
            return (0, 0);
        }
        let (c, _) = self.chain(d, pos, 2);
        if self.near(c, pos) {
            let n = lcp_up(d, c as usize, pos, cap);
            if n >= MIN_MATCH {
                (best, off) = (n, pos - c as usize);
            }
        }
        if cap >= 3 {
            let (mut c, prev) = self.chain(d, pos, 3);
            for _ in 0..DEPTH {
                if !self.near(c, pos) || best >= cap {
                    break;
                }
                let p = c as usize;
                if ends_alike(d, p, pos, best.min(cap - 1)) {
                    let n = lcp_up(d, p, pos, cap);
                    if n > best {
                        (best, off) = (n, pos - p);
                    }
                }
                c = prev[p & self.mask];
            }
        }
        (best, off)
    }

    /// The offsets of matches of at least `len` bytes at `pos`, nearest first, up to [`LIMIT`]
    /// candidates of the chain: calls `f` with each, stopping where it returns true.
    fn each(&self, d: &[u8], pos: usize, len: usize, mut f: impl FnMut(usize) -> bool) {
        let (mut c, prev) = self.chain(d, pos, len);
        for _ in 0..LIMIT {
            if !self.near(c, pos) {
                return;
            }
            let p = c as usize;
            if ends_alike(d, p, pos, len - 1) && lcp_up(d, p, pos, len) >= len && f(pos - p) {
                return;
            }
            c = prev[p & self.mask];
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

/// Bucket of a length: 0 for none (or 1), then 2, 3, 4-5, 6-8, 9-16, 17-32, more.
#[inline]
fn lb(n: usize) -> usize {
    match n {
        0 | 1 => 0,
        2 => 1,
        3 => 2,
        4..=5 => 3,
        6..=8 => 4,
        9..=16 => 5,
        17..=32 => 6,
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
    Tree,
    Rest,
}
pub const CATS: usize = 5;

struct Ctx {
    stored: Bit,
    kind: [Bit; 2 * 4],
    default_size: [Bit; 4],
    whole: [Bit; 4],
    /// A token: a match or not, a repeat or not, which repeat (by the longest match and the
    /// first repeat's).
    is_match: Vec<Bit>,
    is_rep: Vec<Bit>,
    rep0: Vec<Bit>,
    rep1: Vec<Bit>,
    /// Lengths: as predicted or not, shorter or longer, how much (by the kind and length).
    len_eq: Vec<Bit>,
    len_short: Vec<Bit>,
    len_by: Vec<[Bit; 32]>,
    rank: Vec<[Bit; 32]>,
    off_escape: [Bit; 16],
    /// Trees: a length as predicted, else its value (by the prediction); the pretree's choices.
    tree_eq: Vec<Bit>,
    tree_val: Vec<[Bit; 32]>,
    pre_eq: Vec<Bit>,
    pre_val: Vec<[Bit; 32]>,
    run: Vec<Bit>,
    run_kind: Vec<Bit>,
    run_full: Vec<Bit>,
    run_n: [Bit; 32],
    same5: Vec<Bit>,
    /// The rest.
    zero: [Bit; 8],
    gamma: [Bit; 32],
}

/// The vectors of `Ctx` in the order they are made: how many elements, of how many adaptive
/// bits each (a gamma's table has 32).
pub const CTX_VECS: [(usize, usize); 16] = [
    (8 * 8 * 4, 1),
    (8 * 8 * 4, 1),
    (8 * 8, 1),
    (8 * 8, 1),
    (4 * 8 * 8, 1),
    (4 * 8, 1),
    (2 * 4 * 8, 32),
    (8 * 4, 32),
    (17 * 3 * 2, 1),
    (17 * 3, 32),
    (17, 1),
    (17, 32),
    (8 * 8 * 2 * 3, 1),
    (8 * 8 * 2, 1),
    (4, 1),
    (4, 1),
];

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

/// Bytes the tables' work of a block takes at most (Huffman's lists, the trees' symbols and the
/// codes written): with room for the vectors that grow, and for the holes that leaves. The
/// decoder takes them from the stack once before the blocks and gives them back, so that what it
/// takes does not depend on the data.
pub const SCRATCH: usize = 128 << 10;

impl Ctx {
    fn new() -> Ctx {
        let v = |i: usize| vec![Bit::default(); CTX_VECS[i].0];
        let g = |i: usize| vec![[Bit::default(); 32]; CTX_VECS[i].0];
        Ctx {
            stored: Bit::default(),
            kind: Default::default(),
            default_size: Default::default(),
            whole: Default::default(),
            is_match: v(0),
            is_rep: v(1),
            rep0: v(2),
            rep1: v(3),
            len_eq: v(4),
            len_short: v(5),
            len_by: g(6),
            rank: g(7),
            off_escape: Default::default(),
            tree_eq: v(8),
            tree_val: g(9),
            pre_eq: v(10),
            pre_val: g(11),
            run: v(12),
            run_kind: v(13),
            run_full: v(14),
            run_n: Default::default(),
            same5: v(15),
            zero: Default::default(),
            gamma: Default::default(),
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

/// What a walker is made of that survives a stream, and is kept for the next one: the match
/// finder and a chunk's data (allocated once: streams are many, and the allocator slow to find
/// room where much is held; in a module, once for all its streams).
pub struct Spare {
    mf: Option<Box<Mf>>,
    d: Vec<u8>,
}

impl Spare {
    /// A spare for streams of a window of `window` bytes, chunks of data of at most `d_cap` bytes.
    pub fn new(window: usize, d_cap: usize) -> Spare {
        Spare { mf: Some(Box::new(Mf::new(window))), d: Vec::with_capacity(d_cap) }
    }

    /// This thread's last spare, if it is one for `window`.
    fn parked(window: usize) -> Option<Spare> {
        SPARE.with(|s| s.borrow_mut().take()).filter(|s| s.mf.as_ref().is_some_and(|mf| mf.prev2.len() == window))
    }

    /// Parks the spare for this thread's next stream (a module keeps its own: none there).
    fn park(self) {
        if cfg!(not(target_family = "wasm")) {
            SPARE.with(|s| *s.borrow_mut() = Some(self));
        }
    }
}

thread_local! {
    static SPARE: std::cell::RefCell<Option<Spare>> = const { std::cell::RefCell::new(None) };
}

/// The most that the tokens and the frames' padding of a block come to in a stream, which a
/// decoder reserves: tokens, and the padding of frames.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Caps {
    pub toks: usize,
    pub pads: usize,
}

/// Walks a stream's chunks over `data`, coding them (`chunks` given, each its parse or None
/// where stored) or decoding them (written onto `out`, a block at a time), as the side is; or a
/// CAB stream.
struct Walker {
    ctx: Ctx,
    spare: Spare,
    v: Variant,
    /// The code lengths before the block (the trees' of the walk; the writer has its own).
    lens: lzx::Lens,
    wr: lzx::BlockWriter,
    /// The tokens and the frames' padding of the block.
    toks: Vec<Tok>,
    pads: Vec<u16>,
    /// Whether they stay within their capacity (a decoder's: the record's counts).
    fixed: bool,
    /// Where the frame ends, and the stream (CAB's).
    frame_end: usize,
}

/// Where the record is one of an encoder: the chunks' parses to code.
pub type Chunks = Vec<Option<Parse>>;

impl Walker {
    /// A walker of streams of `v` on `spare`; with `caps` the block's tokens and padding stay
    /// within them, else they grow.
    fn new(spare: Spare, v: Variant, caps: Option<Caps>) -> Walker {
        let c = caps.unwrap_or_default();
        Walker {
            ctx: Ctx::new(),
            spare,
            v,
            lens: lzx::Lens::new(v),
            wr: lzx::BlockWriter::new(v),
            toks: Vec::with_capacity(c.toks),
            pads: Vec::with_capacity(c.pads),
            fixed: caps.is_some(),
            frame_end: usize::MAX,
        }
    }

    /// A walker on this thread's spare (else a new one).
    fn parked(v: Variant, caps: Option<Caps>) -> Walker {
        let window = v.window();
        let spare = Spare::parked(window).unwrap_or_else(|| Spare::new(window, CHUNK));
        Walker::new(spare, v, caps)
    }

    /// Done: the spare, for the next stream.
    fn into_spare(self) -> Spare {
        self.spare
    }

    /// Done: the spare left for this thread's next stream.
    fn park(self) {
        self.into_spare().park();
    }
}

impl Walker {
    /// One chunk (or CAB stream) of data `raw` (as the file holds it): its parse coded or
    /// decoded, the chunk written onto `out` a block at a time when decoding.
    fn chunk(
        &mut self,
        side: &mut impl Side,
        raw: &[u8],
        want: Option<&Option<Parse>>,
        out: Option<&mut Vec<u8>>,
    ) -> Result<()> {
        let cab = self.v.cab;
        let mut out = out;
        if !cab {
            let stored = side.bit(&mut self.ctx.stored, matches!(want, Some(None)), Cat::Rest);
            if stored {
                if let Some(out) = out {
                    out.extend_from_slice(raw);
                }
                return Ok(());
            }
        }
        let want = want.map(|w| w.as_ref().ok_or(Error("a stream stored"))).transpose()?;
        let same = Error("a parse the record does not give back");
        // the header (CAB's): the e8 translation's size
        let mut e8 = None;
        if cab {
            let we = want.and_then(|w| w.e8);
            if side.bit(&mut self.ctx.zero[5], we.is_some(), Cat::Rest) {
                let v = we.unwrap_or(0);
                e8 = Some(side.raw(16, v >> 16, Cat::Rest) << 16 | side.raw(16, v & 0xffff, Cat::Rest));
            }
        }
        let mut d = std::mem::take(&mut self.spare.d);
        d.clear();
        d.extend_from_slice(raw);
        match (cab, e8) {
            (false, _) => lzx::e8_encode(&mut d),
            (true, Some(size)) => lzx::e8_encode_frames(&mut d, size),
            (true, None) => {}
        }
        let main = self.v.main();
        self.spare.mf.as_mut().expect("a match finder").reset(if cab { self.v.max_off() } else { CHUNK });
        self.frame_end = if cab { lzx::FRAME.min(d.len()) } else { usize::MAX };
        let mut reps = lzx::REPS;
        let mut lens = std::mem::replace(&mut self.lens, lzx::Lens { main: Vec::new(), len: [0; LEN] });
        lens.main.fill(0);
        lens.len = [0; LEN];
        // (the blocks are written as they are made)
        let mut w = out.as_deref_mut().map(|o| BitW::new(std::mem::take(o)));
        self.wr.start(d.len());
        if let Some(w) = w.as_mut() {
            self.wr.head(w, e8).ok_or_else(corrupt)?;
        }
        let (mut at, mut prev_kind, mut last_size) = (0, 0, lzx::FRAME);
        // (how far the parse given is gone through: tokens, frames' padding, blocks)
        let (mut t, mut fp, mut nb) = (0, 0, 0);
        while at < d.len() {
            let wb = want.map(|w| w.blocks.get(nb).ok_or_else(corrupt)).transpose()?;
            let mut b = Block::default();
            // the type
            let k = wb.map_or(0, |b| b.kind as usize - 1);
            let c = &mut self.ctx.kind[prev_kind * 2..prev_kind * 2 + 2];
            let k = if side.bit(&mut c[0], k > 0, Cat::Rest) {
                1 + side.bit(&mut c[1], k > 1, Cat::Rest) as usize
            } else {
                0
            };
            b.kind = k as u8 + 1;
            if b.kind > UNCOMPRESSED {
                return Err(corrupt());
            }
            prev_kind = k + 1;
            // the size: (WIM's) the default, all that is left, (CAB's) the last block's, or as it is
            let left = d.len() - at;
            if !cab {
                b.default_size = side.bit(&mut self.ctx.default_size[k], wb.is_some_and(|b| b.default_size), Cat::Rest);
            }
            b.size = if b.default_size {
                WINDOW
            } else if side.bit(&mut self.ctx.whole[k], wb.is_some_and(|b| b.size == left), Cat::Rest) {
                left
            } else if cab && side.bit(&mut self.ctx.default_size[k], wb.is_some_and(|b| b.size == last_size), Cat::Rest)
            {
                last_size
            } else {
                let v = wb.map_or(0, |b| b.size as u32);
                let v = match cab {
                    true => (side.raw(8, v >> 16, Cat::Rest) << 16 | side.raw(16, v & 0xffff, Cat::Rest)) as usize,
                    false => side.raw(16, v, Cat::Rest) as usize,
                };
                if v == 0 {
                    return Err(corrupt());
                }
                v
            };
            last_size = b.size;
            let end = (at + b.size).min(d.len());
            self.toks.clear();
            self.pads.clear();
            if b.kind == UNCOMPRESSED {
                let wv = wb.map_or(0, |b| b.align_bits as u32);
                b.align_bits = if side.bit(&mut self.ctx.zero[0], wv != 0, Cat::Rest) {
                    side.raw(16, wv, Cat::Rest) as u16
                } else {
                    0
                };
                let same_reps = wb.is_none_or(|b| b.reps == reps);
                if side.bit(&mut self.ctx.zero[1], same_reps, Cat::Rest) {
                    b.reps = reps;
                } else {
                    for (i, r) in b.reps.iter_mut().enumerate() {
                        let v = wb.map_or(0, |b| b.reps[i]);
                        *r = side.raw(16, v >> 16, Cat::Rest) << 16 | side.raw(16, v & 0xffff, Cat::Rest);
                    }
                }
                if cab && b.reps.iter().any(|&r| r == 0 || r as usize > self.v.max_off()) {
                    return Err(corrupt());
                }
                reps = b.reps;
                if b.size % 2 == 1 {
                    let wp = wb.and_then(|b| b.pad_byte);
                    if side.bit(&mut self.ctx.zero[2], wp.is_some(), Cat::Rest) {
                        let v = wp.unwrap_or(0) as u32;
                        let nz = side.bit(&mut self.ctx.zero[3], v != 0, Cat::Rest);
                        b.pad_byte = Some(if nz { side.raw(8, v, Cat::Rest) as u8 } else { 0 });
                    }
                }
                at = end;
                while self.frame_end <= at && self.frame_end < d.len() {
                    self.frame_end = (self.frame_end + lzx::FRAME).min(d.len());
                }
            } else {
                // the tokens
                let wt = want.map(|w| (&w.toks[..], &w.frame_pad[..]));
                self.tokens(side, &d, at, end, &mut reps, wt, &mut t, &mut fp)?;
                b.toks = self.toks.len();
                // the trees, against the code of the block's frequencies
                let (mut fm, mut fl, mut fa) = ([0u32; MAIN_MAX], [0u32; LEN], [0u32; ALIGNED]);
                let mut p = at;
                for tok in &self.toks {
                    match *tok {
                        Tok::Lit => {
                            fm[d[p] as usize] += 1;
                            p += 1;
                        }
                        Tok::Match { len, off, rep } => {
                            let len = len as usize;
                            let header = (len - MIN_MATCH).min(7);
                            let slot = match rep {
                                Some(k) => k as usize,
                                None => lzx::slot(off + 2),
                            };
                            fm[256 + slot * 8 + header] += 1;
                            if header == 7 {
                                fl[len - MIN_MATCH - 7] += 1;
                            }
                            if rep.is_none() && lzx::EXTRA[slot] >= 3 {
                                fa[((off + 2 - lzx::BASE[slot]) & 7) as usize] += 1;
                            }
                            p += len;
                        }
                    }
                }
                if b.kind == ALIGNED_BLOCK {
                    let mut pred = [0u8; ALIGNED];
                    huffman(&fa, 7, &mut pred);
                    for (i, &p) in pred.iter().enumerate() {
                        let v = wb.map_or(0, |b| b.aligned[i]);
                        b.aligned[i] = self.length(side, p, v, 2, 7)?;
                    }
                }
                let (mut pm, mut pl) = ([0u8; MAIN_MAX], [0u8; LEN]);
                huffman(&fm[..main], 16, &mut pm[..main]);
                huffman(&fl, 16, &mut pl);
                // (the lengths the block ends with, of the parse given)
                let (wm, wl) = match want {
                    Some(w) => want_lens(w, nb, self.v),
                    None => (Vec::new(), [0; LEN]),
                };
                let mut target_m = vec![0u8; main];
                let mut target_l = [0u8; LEN];
                for i in 0..main {
                    target_m[i] = self.length(side, pm[i], wm.get(i).copied().unwrap_or(0), 0, 16)?;
                }
                for i in 0..LEN {
                    target_l[i] = self.length(side, pl[i], wl[i], 1, 16)?;
                }
                b.trees[0] = self.tree(side, &lens.main[..256], &target_m[..256], wb.map(|b| &b.trees[0]))?;
                b.trees[1] = self.tree(side, &lens.main[256..], &target_m[256..], wb.map(|b| &b.trees[1]))?;
                b.trees[2] = self.tree(side, &lens.len, &target_l, wb.map(|b| &b.trees[2]))?;
                lens.main.copy_from_slice(&target_m);
                lens.len = target_l;
                at = end;
            }
            if let Some(w) = w.as_mut() {
                let used = self.wr.block(w, &b, &self.toks, &self.pads, &d).ok_or_else(corrupt)?;
                if used != self.pads.len() {
                    return Err(corrupt());
                }
            }
            if let Some(pw) = want {
                let (n, p) = (self.toks.len(), self.pads.len());
                let block = *wb.expect("a block given") == b;
                if !block
                    || pw.toks.get(t - n..t) != Some(&self.toks[..])
                    || pw.frame_pad.get(fp - p..fp) != Some(&self.pads[..])
                {
                    return Err(same);
                }
            }
            nb += 1;
        }
        self.lens = lens;
        // the last word's padding, and bytes after it
        let wt = want.map_or(0, |w| w.tail_bits as u32);
        let tail_bits =
            if side.bit(&mut self.ctx.zero[4], wt != 0, Cat::Rest) { side.raw(16, wt, Cat::Rest) as u16 } else { 0 };
        if let Some(w) = w.as_mut() {
            self.wr.tail(w, tail_bits).ok_or_else(corrupt)?;
        }
        let wn = want.map_or(0, |w| w.trailing.len() as u32);
        let n = side.gamma(&mut self.ctx.gamma, wn, Cat::Rest)? as usize;
        if n > CHUNK {
            return Err(corrupt());
        }
        for i in 0..n {
            let v = want.map_or(0, |w| w.trailing[i] as u32);
            let byte = side.raw(8, v, Cat::Rest) as u8;
            if let Some(w) = w.as_mut() {
                w.out.push(byte);
            }
        }
        if let Some(pw) = want
            && (pw.e8 != e8
                || nb != pw.blocks.len()
                || t != pw.toks.len()
                || fp != pw.frame_pad.len()
                || pw.tail_bits != tail_bits)
        {
            return Err(same);
        }
        if let (Some(o), Some(w)) = (out, w) {
            *o = w.out;
        }
        self.spare.d = d;
        Ok(())
    }

    /// A code length, against `pred`, of at most `max`.
    fn length(&mut self, side: &mut impl Side, pred: u8, v: u8, tree: usize, max: u8) -> Result<u8> {
        let c = &mut self.ctx;
        let eq = side.bit(&mut c.tree_eq[(pred as usize * 3 + tree) * 2], v == pred, Cat::Tree);
        if eq {
            return Ok(pred);
        }
        let x = side.tree5(&mut c.tree_val[pred as usize * 3 + tree], v as u32, Cat::Tree) as u8;
        if x > max || x == pred {
            return Err(corrupt());
        }
        Ok(x)
    }

    /// How the pretree writes `target` over `before`: the pretree's symbols, coded under the
    /// runs there are, then its lengths against a Huffman code of the symbols.
    fn tree(
        &mut self,
        side: &mut impl Side,
        before: &[u8],
        target: &[u8],
        want: Option<&TreeCode>,
    ) -> Result<TreeCode> {
        let mut t = TreeCode::default();
        let c = &mut self.ctx;
        let (mut i, mut k) = (0, 0);
        while i < target.len() {
            let ws = want.map(|w| w.syms.get(k).copied().ok_or_else(corrupt)).transpose()?;
            k += 1;
            let zeros = target[i..].iter().take_while(|&&l| l == 0).count();
            let same = target[i..].iter().take_while(|&&l| l == target[i]).count();
            let ctx = (lb(zeros) * 8 + lb(same)) * 2 + (target[i] == 0) as usize;
            // a run or a single length
            let is_run = ws.is_some_and(|s| s.sym >= 17);
            let can = zeros >= 4 || same >= 4;
            let run = can && side.bit(&mut c.run[ctx * 3], is_run, Cat::Tree);
            if !run {
                let sym = (before[i] + 17 - target[i]) % 17;
                if ws.is_some_and(|s| s.sym != sym) {
                    return Err(Error("a pretree symbol not as its lengths say"));
                }
                t.syms.push(PreSym { sym, extra: 0, then: 0 });
                i += 1;
                continue;
            }
            // of zeros (17 or 18), or the same (19)
            let s = if zeros >= 4 {
                let want_z = ws.is_some_and(|s| s.sym != 19);
                if same < 4 || side.bit(&mut c.run_kind[ctx], want_z, Cat::Tree) { 0 } else { 19 }
            } else {
                19
            };
            let sym = if s == 19 {
                19
            } else {
                // 17 (4 to 19) or 18 (20 to 51)
                let w18 = ws.is_some_and(|s| s.sym == 18);
                if zeros < 20 {
                    17
                } else if side.bit(&mut c.run[ctx * 3 + 1], w18, Cat::Tree) {
                    18
                } else {
                    17
                }
            };
            let (min, max) = match sym {
                17 => (4, 19),
                18 => (20, 51),
                _ => (4, 5),
            };
            let avail = if sym == 19 { same } else { zeros };
            let full = avail.min(max);
            let wn = ws.map_or(0, |s| match s.sym {
                17 => 4 + s.extra as usize,
                18 => 20 + s.extra as usize,
                _ => 4 + s.extra as usize,
            });
            let n = if sym == 19 {
                if full < 5 { 4 } else { 4 + side.bit(&mut c.same5[(lb(same)).min(3)], wn == 5, Cat::Tree) as usize }
            } else if side.bit(&mut c.run_full[(sym - 17) as usize], wn == full, Cat::Tree) {
                full
            } else {
                min + side.tree5(&mut c.run_n, wn.saturating_sub(min) as u32, Cat::Tree) as usize
            };
            if n < min || n > full.max(min) || i + n > target.len() {
                return Err(corrupt());
            }
            let extra = (n - min) as u8;
            let then = if sym == 19 { (before[i] + 17 - target[i]) % 17 } else { 0 };
            if let Some(w) = ws
                && (w.sym != sym || w.extra != extra || w.then != then)
            {
                return Err(Error("a pretree run not as its lengths say"));
            }
            t.syms.push(PreSym { sym, extra, then });
            i += n;
        }
        // the pretree's lengths
        let mut freqs = [0u32; PRE];
        for s in &t.syms {
            freqs[s.sym as usize] += 1;
            if s.sym == 19 {
                freqs[s.then as usize] += 1;
            }
        }
        let mut pred = [0u8; PRE];
        huffman(&freqs, 15, &mut pred);
        for (s, &p) in pred.iter().enumerate() {
            let v = want.map_or(0, |w| w.pre[s]);
            let eq = side.bit(&mut c.pre_eq[p as usize], v == p, Cat::Tree);
            t.pre[s] = if eq {
                p
            } else {
                let x = side.tree5(&mut c.pre_val[p as usize], v as u32, Cat::Tree) as u8;
                if x > 15 {
                    return Err(corrupt());
                }
                x
            };
        }
        Ok(t)
    }

    /// The tokens of the block of `d[at..end)`: coded from `want` (its tokens from the `t`-th,
    /// and its frames' padding) or decoded, onto `parse`.
    #[allow(clippy::too_many_arguments)]
    fn tokens(
        &mut self,
        side: &mut impl Side,
        d: &[u8],
        at: usize,
        end: usize,
        reps: &mut [u32; 3],
        want: Option<(&[Tok], &[u16])>,
        t: &mut usize,
        fp: &mut usize,
    ) -> Result<()> {
        let c = &mut self.ctx;
        let mf = self.spare.mf.as_deref_mut().expect("a match finder");
        let (want, want_pads) = (want.map(|w| w.0), want.map(|w| w.1));
        let esc_bits = if self.v.cab { self.v.window_bits as u32 } else { 15 };
        let (toks, pads, fixed) = (&mut self.toks, &mut self.pads, self.fixed);
        let frame_end = &mut self.frame_end;
        let mut pos = at;
        while pos < end {
            mf.advance(d, pos);
            let cap = (end - pos).min(MAX_MATCH);
            let w = want.map(|w| w.get(*t).copied().ok_or_else(corrupt)).transpose()?;
            *t += 1;
            let (longest, _) = mf.longest(d, pos, cap);
            let rl: [usize; 3] = std::array::from_fn(|k| {
                if (reps[k] as usize) <= pos { lcp_up(d, pos - reps[k] as usize, pos, cap) } else { 0 }
            });
            let ctx = (lb(longest) * 8 + lb(rl[0])) * 4 + lb(rl[1]).min(3);
            let is_match = side.bit(&mut c.is_match[ctx], matches!(w, Some(Tok::Match { .. })), Cat::Kind);
            if toks.len() == toks.capacity() && fixed {
                return Err(Error("more tokens than the record counts"));
            }
            if !is_match {
                toks.push(Tok::Lit);
                pos += 1;
                pad(side, &mut c.zero[6], frame_end, d.len(), pos, (pads, fp, fixed), want_pads)?;
                continue;
            }
            let (wlen, woff, wrep) = match w {
                Some(Tok::Match { len, off, rep }) => (len as usize, off as usize, rep),
                _ => (0, 0, None),
            };
            let rep = if rl.iter().any(|&l| l >= MIN_MATCH) {
                if side.bit(&mut c.is_rep[ctx], wrep.is_some(), Cat::Kind) {
                    let r0 = side.bit(&mut c.rep0[lb(longest) * 8 + lb(rl[0])], wrep == Some(0), Cat::Kind);
                    Some(if r0 {
                        0
                    } else {
                        1 + !side.bit(&mut c.rep1[lb(rl[1]) * 8 + lb(rl[2])], wrep == Some(1), Cat::Kind) as u8
                    })
                } else {
                    None
                }
            } else {
                None
            };
            // the length, against the repeat's or the longest match's
            let (kind, pred) = match rep {
                Some(k) => (k as usize, rl[k as usize]),
                None => (3, longest),
            };
            let len = self_len(side, c, kind, pred, wlen, cap)?;
            let off = match rep {
                Some(k) => {
                    reps.swap(0, k as usize);
                    reps[0] as usize
                }
                None => {
                    // the offset's rank among those giving the length, nearest first
                    let mut rank = 0u32;
                    let mut found = false;
                    if want.is_some() {
                        mf.each(d, pos, len, |o| {
                            if o == woff {
                                found = true;
                                return true;
                            }
                            rank += 1;
                            false
                        });
                    }
                    let esc = side.bit(&mut c.off_escape[lb(len)], want.is_some() && !found, Cat::Off);
                    let off = if esc {
                        side.raw(esc_bits, woff as u32, Cat::Off) as usize
                    } else {
                        let rank = side.gamma(&mut c.rank[lb(len) * 4 + lb(longest).min(3)], rank, Cat::Off)?;
                        // (split: the offset of the rank is the one found for it)
                        if want.is_some() {
                            woff
                        } else {
                            let mut n = 0;
                            let mut off = 0;
                            mf.each(d, pos, len, |o| {
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
                    if off == 0 || off > pos.min(mf.max_off) {
                        return Err(corrupt());
                    }
                    *reps = [off as u32, reps[0], reps[1]];
                    off
                }
            };
            if off == 0 || off > pos || len > cap || lcp_up(d, pos - off, pos, len) < len {
                return Err(corrupt());
            }
            toks.push(Tok::Match { len: len as u16, off: off as u32, rep });
            pos += len;
            pad(side, &mut c.zero[6], frame_end, d.len(), pos, (pads, fp, fixed), want_pads)?;
        }
        Ok(())
    }
}

/// (CAB's) The padding of the frame a token ends at `pos` (where the frame ends before `n`),
/// against `want`, onto `pads`.
fn pad(
    side: &mut impl Side,
    bit: &mut Bit,
    frame_end: &mut usize,
    n: usize,
    pos: usize,
    (pads, fp, fixed): (&mut Vec<u16>, &mut usize, bool),
    want: Option<&[u16]>,
) -> Result<()> {
    if pos >= *frame_end && *frame_end < n {
        let wp = want.map(|p| p.get(*fp).copied().ok_or_else(corrupt)).transpose()?.unwrap_or(0);
        let v = if side.bit(bit, wp != 0, Cat::Rest) { side.raw(16, wp as u32, Cat::Rest) } else { 0 };
        if fixed && pads.len() == pads.capacity() {
            return Err(Error("more frames than the record counts"));
        }
        pads.push(v as u16);
        *fp += 1;
        *frame_end = (*frame_end + lzx::FRAME).min(n);
    }
    Ok(())
}

/// A match's length against `pred`, of a match of `kind` (a repeat 0-2, or 3 for new), of at
/// most `cap`.
fn self_len(side: &mut impl Side, c: &mut Ctx, kind: usize, pred: usize, want: usize, cap: usize) -> Result<usize> {
    let ctx = kind * 8 + lb(pred);
    if pred >= MIN_MATCH && side.bit(&mut c.len_eq[ctx * 2], want == pred, Cat::Len) {
        return Ok(pred);
    }
    let shorter = pred > MIN_MATCH && (kind < 3 || side.bit(&mut c.len_short[ctx], want < pred, Cat::Len));
    let len = if shorter {
        let v = side.gamma(&mut c.len_by[ctx], (pred.saturating_sub(want + 1)) as u32, Cat::Len)? as usize;
        pred.checked_sub(v + 1).ok_or_else(corrupt)?
    } else {
        let base = pred.max(MIN_MATCH - 1);
        let v = side.gamma(&mut c.len_by[(4 * 8) + ctx], want.saturating_sub(base + 1) as u32, Cat::Len)? as usize;
        base + 1 + v
    };
    if !(MIN_MATCH..=cap).contains(&len) {
        return Err(corrupt());
    }
    Ok(len)
}

/// The code lengths block `b` of `parse` (of `v`) ends with: the main tree's and the length
/// tree's.
fn want_lens(parse: &Parse, b: usize, v: Variant) -> (Vec<u8>, [u8; LEN]) {
    let mut l = lzx::Lens::new(v);
    for blk in &parse.blocks[..=b] {
        if blk.kind != UNCOMPRESSED {
            l.apply(blk).expect("trees read");
        }
    }
    (l.main, l.len)
}

/// The data and record of a stream of chunks, each its bytes and its data's length (a chunk of
/// as many bytes as its data is stored).
pub struct Split {
    pub data: Vec<u8>,
    pub record: Vec<u8>,
    pub cost: [f64; CATS],
}

/// Splits a stream of chunks: None where a chunk does not read whole, or is not [`CHUNK`]
/// bytes of data (but the last).
pub fn split(chunks: &[(&[u8], usize)]) -> Result<Split> {
    let mut data = vec![0; data_len(chunks)];
    let parses = read_into(chunks, &mut data)?;
    let mut enc = Enc::with(ArithEncoder::new(Vec::new()));
    walk(&mut enc, &data, &parses)?;
    Ok(Split { data, record: enc.c.finish(), cost: enc.cost })
}

/// Bytes of the data of a stream of chunks.
pub fn data_len(chunks: &[(&[u8], usize)]) -> usize {
    chunks.iter().map(|c| c.1).sum()
}

/// [`split`] with the data written into `data` (of [`data_len`] bytes), as where a container
/// holds it all: the record (its cost not counted).
pub fn split_into(chunks: &[(&[u8], usize)], data: &mut [u8]) -> Result<Vec<u8>> {
    let parses = read_into(chunks, data)?;
    let mut enc = ArithEncoder::new(Vec::new());
    walk(&mut enc, data, &parses)?;
    Ok(enc.finish())
}

/// The record of the chunks of `data` of `parses` coded onto `enc`.
fn walk(enc: &mut impl Side, data: &[u8], parses: &[Option<Parse>]) -> Result<()> {
    let mut caps = Caps::default();
    for (raw, p) in data.chunks(CHUNK).zip(parses) {
        if let Some(p) = p {
            caps = caps.max(caps_of(p, raw.len(), lzx::WIM));
        }
    }
    code_caps(enc, caps)?;
    let mut w = Walker::parked(lzx::WIM, None);
    let done = (|| {
        for (raw, p) in data.chunks(CHUNK).zip(parses) {
            w.chunk(enc, raw, Some(p), None)?;
        }
        Ok(())
    })();
    w.park();
    done
}

/// The tokens and the frames' padding the most of any block of `parse` of a stream of `n` bytes
/// of data (as translated by e8) come to.
fn caps_of(parse: &Parse, n: usize, v: Variant) -> Caps {
    let mut caps = Caps::default();
    let mut frame_end = if v.cab { lzx::FRAME.min(n) } else { usize::MAX };
    let (mut at, mut t) = (0, 0);
    for b in &parse.blocks {
        let end = (at + b.size).min(n);
        if b.kind == UNCOMPRESSED {
            at = end;
            while frame_end <= at && frame_end < n {
                frame_end = (frame_end + lzx::FRAME).min(n);
            }
            continue;
        }
        let mut pads = 0;
        for tok in parse.toks.get(t..t + b.toks).unwrap_or_default() {
            at += match *tok {
                Tok::Lit => 1,
                Tok::Match { len, .. } => len as usize,
            };
            if at >= frame_end && frame_end < n {
                pads += 1;
                frame_end = (frame_end + lzx::FRAME).min(n);
            }
        }
        t += b.toks;
        caps = Caps { toks: caps.toks.max(b.toks), pads: caps.pads.max(pads) };
    }
    caps
}

impl Caps {
    fn max(self, o: Caps) -> Caps {
        Caps { toks: self.toks.max(o.toks), pads: self.pads.max(o.pads) }
    }
}

/// A record starts with its capacities: the most tokens of a block, the most frames' padding of a
/// block, of the whole stream (two gammas under contexts of their own).
fn code_caps(side: &mut impl Side, caps: Caps) -> Result<Caps> {
    let mut g = [Bit::default(); 32];
    let toks = side.gamma(&mut g, caps.toks as u32, Cat::Rest)? as usize;
    let pads = side.gamma(&mut g, caps.pads as u32, Cat::Rest)? as usize;
    Ok(Caps { toks, pads })
}

/// The capacities a record of `n` bytes of data starts with: a token covers a byte at least, a
/// block's frames are of [`lzx::FRAME`] bytes.
pub fn read_caps(dec: &mut ArithDecoder, n: usize) -> Result<Caps> {
    let caps = code_caps(dec, Caps::default())?;
    if caps.toks > n || caps.pads > n / lzx::FRAME + 2 {
        return Err(corrupt());
    }
    Ok(caps)
}

/// The data of a stream of chunks written into `data` (of [`data_len`] bytes), as [`split_into`]
/// writes it, without the record: the stream's data made again where it was not kept.
pub fn data_into(chunks: &[(&[u8], usize)], data: &mut [u8]) -> Result<()> {
    read_into(chunks, data).map(drop)
}

/// The data of a stream of chunks written into `data`, and the parse of each chunk (none for
/// one stored).
fn read_into(chunks: &[(&[u8], usize)], data: &mut [u8]) -> Result<Vec<Option<lzx::Parse>>> {
    if data.len() != data_len(chunks) {
        return Err(Error("data of another length"));
    }
    let mut parses = Vec::with_capacity(chunks.len());
    let mut at = 0;
    for (i, &(c, n)) in chunks.iter().enumerate() {
        if n == 0 || n > CHUNK || n < CHUNK && i + 1 < chunks.len() {
            return Err(Error("chunks of another size"));
        }
        let out = &mut data[at..at + n];
        at += n;
        if c.len() == n {
            out.copy_from_slice(c);
            parses.push(None);
            continue;
        }
        let (d, p) = lzx::read(c, n).ok_or(Error("a chunk not read"))?;
        out.copy_from_slice(&d);
        lzx::e8_decode(out);
        parses.push(Some(p));
    }
    Ok(parses)
}

/// The stream that `data` and `record` rebuild.
pub fn join(data: &[u8], record: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(data.len() / 2);
    join_onto(data, record, &mut out)?;
    Ok(out)
}

/// [`join`] onto `out`.
pub fn join_onto(data: &[u8], record: &[u8], out: &mut Vec<u8>) -> Result<()> {
    let spare = Spare::parked(lzx::WIM.window()).unwrap_or_else(|| Spare::new(lzx::WIM.window(), CHUNK));
    let (spare, done) = join_in(spare, lzx::WIM, data, record, out);
    spare.park();
    done
}

/// The stream of `v` (WIM's chunks, or a CAB stream) that `data` and `record` rebuild, onto
/// `out` (room for it reserved), on `spare` (chunks of at most its capacity of data): the spare
/// back, to go on with.
pub fn join_in(spare: Spare, v: Variant, data: &[u8], record: &[u8], out: &mut Vec<u8>) -> (Spare, Result<()>) {
    let mut dec = ArithDecoder::new(record);
    let caps = match read_caps(&mut dec, data.len()) {
        Ok(c) => c,
        Err(e) => return (spare, Err(e)),
    };
    let mut w = Walker::new(spare, v, Some(caps));
    // (the work of the blocks' trees takes memory of sizes the tokens decide: all of it within
    // SCRATCH bytes, taken from the stack once before the blocks)
    std::hint::black_box(Vec::<u8>::with_capacity(SCRATCH));
    let done = (|| {
        if v.cab {
            w.chunk(&mut dec, data, None, Some(out))?;
            return if dec.overrun() { Err(corrupt()) } else { Ok(()) };
        }
        for raw in data.chunks(CHUNK) {
            w.chunk(&mut dec, raw, None, Some(out))?;
            if dec.overrun() {
                return Err(corrupt());
            }
        }
        Ok(())
    })();
    (w.into_spare(), done)
}

/// The data and record of stream `src` of `v` (CAB's) of `n` bytes of data. None where it does
/// not read whole.
pub fn split_stream(src: &[u8], n: usize, v: Variant) -> Result<Split> {
    let (mut data, parse) = lzx::read_as(src, n, v).ok_or(Error("a stream not read"))?;
    if let Some(size) = parse.e8 {
        lzx::e8_decode_frames(&mut data, size);
    }
    let mut enc = Enc::with(ArithEncoder::new(Vec::new()));
    code_caps(&mut enc, caps_of(&parse, data.len(), v))?;
    let mut w = Walker::parked(v, None);
    let done = w.chunk(&mut enc, &data, Some(&Some(parse)), None);
    w.park();
    done?;
    Ok(Split { data, record: enc.c.finish(), cost: enc.cost })
}

/// The stream of `v` that `data` and `record` rebuild, onto `out`.
pub fn join_stream_onto(data: &[u8], record: &[u8], v: Variant, out: &mut Vec<u8>) -> Result<()> {
    let spare = Spare::parked(v.window()).unwrap_or_else(|| Spare::new(v.window(), CHUNK));
    let (spare, done) = join_in(spare, v, data, record, out);
    spare.park();
    done
}
