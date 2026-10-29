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
    self, ALIGNED, ALIGNED_BLOCK, BitW, Block, LEN, MAIN, MAX_MATCH, MIN_MATCH, PRE, Parse, PreSym, Tok, TreeCode,
    UNCOMPRESSED, WINDOW,
};
use crate::{Error, Result, corrupt};

/// Bytes of the data of a chunk.
pub const CHUNK: usize = WINDOW;
/// Candidates looked at for the longest match, on the chain of 3 bytes.
pub const DEPTH: usize = 32;
/// Candidates looked at for an offset's rank; past them, the offset is written as it is.
pub const LIMIT: usize = 4096;

const NONE: u32 = u32::MAX;

/// The length of the common prefix of `d[a..]` and `d[b..]`, `a < b`, up to `cap` bytes.
#[inline]
fn lcp(d: &[u8], a: usize, b: usize, cap: usize) -> usize {
    let n = cap.min(d.len() - b);
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

#[inline]
fn k2(d: &[u8], p: usize) -> usize {
    d[p] as usize | (d[p + 1] as usize) << 8
}

#[inline]
fn h3(d: &[u8], p: usize) -> usize {
    let k = d[p] as u32 | (d[p + 1] as u32) << 8 | (d[p + 2] as u32) << 16;
    (k.wrapping_mul(0x9e37_79b1) >> 16) as usize
}

/// The match finder of a chunk: every position before the current one, on a chain of its
/// first 2 bytes and one of a hash of its first 3.
struct Mf {
    head2: Vec<u32>,
    prev2: Vec<u32>,
    head3: Vec<u32>,
    prev3: Vec<u32>,
    next: usize,
}

impl Mf {
    fn new() -> Mf {
        Mf {
            head2: vec![NONE; 1 << 16],
            prev2: vec![NONE; CHUNK],
            head3: vec![NONE; 1 << 16],
            prev3: vec![NONE; CHUNK],
            next: 0,
        }
    }

    fn reset(&mut self) {
        self.head2.fill(NONE);
        self.head3.fill(NONE);
        self.next = 0;
    }

    /// Inserts the positions before `pos`.
    #[inline]
    fn advance(&mut self, d: &[u8], pos: usize) {
        while self.next < pos {
            let p = self.next;
            if p + 3 <= d.len() {
                let h = h3(d, p);
                self.prev3[p] = self.head3[h];
                self.head3[h] = p as u32;
            }
            if p + 2 <= d.len() {
                let k = k2(d, p);
                self.prev2[p] = self.head2[k];
                self.head2[k] = p as u32;
            }
            self.next += 1;
        }
    }

    /// The chain a match of `len` bytes at `pos` is on: its first candidate and its links.
    #[inline]
    fn chain(&self, d: &[u8], pos: usize, len: usize) -> (u32, &[u32]) {
        if len >= 3 && pos + 3 <= d.len() {
            (self.head3[h3(d, pos)], &self.prev3)
        } else if pos + 2 <= d.len() {
            (self.head2[k2(d, pos)], &self.prev2)
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
        if c != NONE {
            let n = lcp(d, c as usize, pos, cap);
            if n >= MIN_MATCH {
                (best, off) = (n, pos - c as usize);
            }
        }
        if cap >= 3 {
            let (mut c, prev) = self.chain(d, pos, 3);
            for _ in 0..DEPTH {
                if c == NONE || best >= cap {
                    break;
                }
                let p = c as usize;
                if d[p + best.min(cap - 1)] == d[pos + best.min(cap - 1)] {
                    let n = lcp(d, p, pos, cap);
                    if n > best {
                        (best, off) = (n, pos - p);
                    }
                }
                c = prev[p];
            }
        }
        (best, off)
    }

    /// The offsets of matches of at least `len` bytes at `pos`, nearest first, up to [`LIMIT`]
    /// candidates of the chain: calls `f` with each, stopping where it returns true.
    fn each(&self, d: &[u8], pos: usize, len: usize, mut f: impl FnMut(usize) -> bool) {
        let (mut c, prev) = self.chain(d, pos, len);
        for _ in 0..LIMIT {
            if c == NONE {
                return;
            }
            let p = c as usize;
            if d[p + len - 1] == d[pos + len - 1] && lcp(d, p, pos, len) >= len && f(pos - p) {
                return;
            }
            c = prev[p];
        }
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

impl Ctx {
    fn new() -> Ctx {
        let v = |n| vec![Bit::default(); n];
        Ctx {
            stored: Bit::default(),
            kind: Default::default(),
            default_size: Default::default(),
            whole: Default::default(),
            is_match: v(8 * 8 * 4),
            is_rep: v(8 * 8 * 4),
            rep0: v(8 * 8),
            rep1: v(8 * 8),
            len_eq: v(4 * 8 * 8),
            len_short: v(4 * 8),
            len_by: vec![[Bit::default(); 32]; 2 * 4 * 8],
            rank: vec![[Bit::default(); 32]; 8 * 4],
            off_escape: Default::default(),
            tree_eq: v(17 * 3 * 2),
            tree_val: vec![[Bit::default(); 32]; 17 * 3],
            pre_eq: v(17),
            pre_val: vec![[Bit::default(); 32]; 17],
            run: v(8 * 8 * 2 * 3),
            run_kind: v(8 * 8 * 2),
            run_full: v(4),
            run_n: Default::default(),
            same5: v(4),
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
    fn raw(&mut self, n: u32, v: u32, cat: Cat) -> u32 {
        let mut x = 0;
        for i in (0..n).rev() {
            x = x << 1 | self.bit(&mut Bit::default(), v >> i & 1 != 0, cat) as u32;
        }
        x
    }

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

pub struct Enc {
    c: ArithEncoder,
    pub cost: [f64; CATS],
}

impl Side for Enc {
    #[inline]
    fn bit(&mut self, b: &mut Bit, v: bool, cat: Cat) -> bool {
        let p1 = b.p1();
        self.cost[cat as usize] -= ((if v { p1 } else { 65536 - p1 }) as f64 / 65536.0).log2();
        self.c.encode(v, p1);
        b.update(v);
        v
    }
}

struct Dec<'a> {
    c: ArithDecoder<'a>,
}

impl Side for Dec<'_> {
    #[inline]
    fn bit(&mut self, b: &mut Bit, _: bool, _: Cat) -> bool {
        let v = self.c.decode(b.p1());
        b.update(v);
        v
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

/// Walks a stream's chunks over `data`, coding them (`chunks` given, each its parse or None
/// where stored) or decoding them (written onto `out`), as the side is.
struct Walker {
    ctx: Ctx,
    mf: Mf,
}

/// Where the record is one of an encoder: the chunks' parses to code.
pub type Chunks = Vec<Option<Parse>>;

impl Walker {
    fn new() -> Walker {
        Walker { ctx: Ctx::new(), mf: Mf::new() }
    }

    /// One chunk of data `raw` (as the file holds it): its parse coded or decoded, the chunk
    /// written onto `out` when decoding.
    fn chunk(
        &mut self,
        side: &mut impl Side,
        raw: &[u8],
        want: Option<&Option<Parse>>,
        out: Option<&mut Vec<u8>>,
    ) -> Result<()> {
        let stored = side.bit(&mut self.ctx.stored, matches!(want, Some(None)), Cat::Rest);
        if stored {
            if let Some(out) = out {
                out.extend_from_slice(raw);
            }
            return Ok(());
        }
        let mut d = raw.to_vec();
        lzx::e8_encode(&mut d);
        let want = want.map(|w| w.as_ref().expect("a parse"));
        let mut parse = Parse::default();
        self.mf.reset();
        let mut reps = lzx::REPS;
        let mut lens = lzx::Lens::default();
        let (mut at, mut prev_kind) = (0, 0);
        let mut t = 0;
        while at < d.len() {
            let wb = want.map(|w| w.blocks.get(parse.blocks.len()).ok_or_else(corrupt)).transpose()?;
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
            // the size: the default, all that is left, or as it is
            let left = d.len() - at;
            b.default_size = side.bit(&mut self.ctx.default_size[k], wb.is_some_and(|b| b.default_size), Cat::Rest);
            b.size = if b.default_size {
                WINDOW
            } else if side.bit(&mut self.ctx.whole[k], wb.is_some_and(|b| b.size == left), Cat::Rest) {
                left
            } else {
                let v = wb.map_or(0, |b| b.size as u32);
                let v = side.raw(16, v, Cat::Rest) as usize;
                if v == 0 {
                    return Err(corrupt());
                }
                v
            };
            let end = (at + b.size).min(d.len());
            if b.kind == UNCOMPRESSED {
                let wv = wb.map_or(0, |b| b.align_bits as u32);
                b.align_bits = if side.bit(&mut self.ctx.zero[0], wv != 0, Cat::Rest) {
                    side.raw(16, wv, Cat::Rest) as u16
                } else {
                    0
                };
                let same = wb.is_none_or(|b| b.reps == reps);
                if side.bit(&mut self.ctx.zero[1], same, Cat::Rest) {
                    b.reps = reps;
                } else {
                    for (i, r) in b.reps.iter_mut().enumerate() {
                        let v = wb.map_or(0, |b| b.reps[i]);
                        *r = side.raw(16, v >> 16, Cat::Rest) << 16 | side.raw(16, v & 0xffff, Cat::Rest);
                    }
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
                parse.blocks.push(b);
                continue;
            }
            // the tokens
            let first = parse.toks.len();
            let wt = want.map(|w| &w.toks[..]);
            self.tokens(side, &d, at, end, &mut reps, wt, &mut t, &mut parse.toks)?;
            b.toks = parse.toks.len() - first;
            // the trees, against the code of the block's frequencies
            let (mut fm, mut fl, mut fa) = ([0u32; MAIN], [0u32; LEN], [0u32; ALIGNED]);
            let mut p = at;
            for tok in &parse.toks[first..] {
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
                            None => lzx::slot(off as u32 + 2),
                        };
                        fm[256 + slot * 8 + header] += 1;
                        if header == 7 {
                            fl[len - MIN_MATCH - 7] += 1;
                        }
                        if rep.is_none() && lzx::EXTRA[slot] >= 3 {
                            fa[((off as u32 + 2 - lzx::BASE[slot]) & 7) as usize] += 1;
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
            let (mut pm, mut pl) = ([0u8; MAIN], [0u8; LEN]);
            huffman(&fm, 16, &mut pm);
            huffman(&fl, 16, &mut pl);
            let (wm, wl) = match want {
                Some(w) => want_lens(w, parse.blocks.len()),
                None => ([0; MAIN], [0; LEN]),
            };
            let mut target_m = [0u8; MAIN];
            let mut target_l = [0u8; LEN];
            for i in 0..MAIN {
                target_m[i] = self.length(side, pm[i], wm[i], 0, 16)?;
            }
            for i in 0..LEN {
                target_l[i] = self.length(side, pl[i], wl[i], 1, 16)?;
            }
            b.trees[0] = self.tree(side, &lens.main[..256], &target_m[..256], wb.map(|b| &b.trees[0]))?;
            b.trees[1] = self.tree(side, &lens.main[256..], &target_m[256..], wb.map(|b| &b.trees[1]))?;
            b.trees[2] = self.tree(side, &lens.len, &target_l, wb.map(|b| &b.trees[2]))?;
            lens.main = target_m;
            lens.len = target_l;
            at = end;
            parse.blocks.push(b);
        }
        // the last word's padding, and bytes after it
        let wt = want.map_or(0, |w| w.tail_bits as u32);
        parse.tail_bits =
            if side.bit(&mut self.ctx.zero[4], wt != 0, Cat::Rest) { side.raw(16, wt, Cat::Rest) as u16 } else { 0 };
        let wn = want.map_or(0, |w| w.trailing.len() as u32);
        let n = side.gamma(&mut self.ctx.gamma, wn, Cat::Rest)? as usize;
        if n > CHUNK {
            return Err(corrupt());
        }
        for i in 0..n {
            let v = want.map_or(0, |w| w.trailing[i] as u32);
            parse.trailing.push(side.raw(8, v, Cat::Rest) as u8);
        }
        if want.is_some_and(|w| *w != parse) {
            return Err(Error("a parse the record does not give back"));
        }
        if let Some(out) = out {
            let mut w = BitW::new(std::mem::take(out));
            let ok = lzx::write_onto(&d, &parse, &mut w);
            *out = w.out;
            ok.ok_or_else(corrupt)?;
        }
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

    /// The tokens of the block of `d[at..end)`: coded from `want` (from its `t`-th) or decoded,
    /// onto `toks`.
    #[allow(clippy::too_many_arguments)]
    fn tokens(
        &mut self,
        side: &mut impl Side,
        d: &[u8],
        at: usize,
        end: usize,
        reps: &mut [u32; 3],
        want: Option<&[Tok]>,
        t: &mut usize,
        toks: &mut Vec<Tok>,
    ) -> Result<()> {
        let c = &mut self.ctx;
        let mf = &mut self.mf;
        let mut pos = at;
        while pos < end {
            mf.advance(d, pos);
            let cap = (end - pos).min(MAX_MATCH);
            let w = want.map(|w| w.get(*t).copied().ok_or_else(corrupt)).transpose()?;
            *t += 1;
            let (longest, _) = mf.longest(d, pos, cap);
            let rl: [usize; 3] = std::array::from_fn(|k| {
                if (reps[k] as usize) <= pos { lcp(d, pos - reps[k] as usize, pos, cap) } else { 0 }
            });
            let ctx = (lb(longest) * 8 + lb(rl[0])) * 4 + lb(rl[1]).min(3);
            let is_match = side.bit(&mut c.is_match[ctx], matches!(w, Some(Tok::Match { .. })), Cat::Kind);
            if !is_match {
                toks.push(Tok::Lit);
                pos += 1;
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
                        side.raw(15, woff as u32, Cat::Off) as usize
                    } else {
                        let rank = side.gamma(&mut c.rank[lb(len) * 4 + lb(longest).min(3)], rank, Cat::Off)?;
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
                    };
                    if off == 0 || off > pos {
                        return Err(corrupt());
                    }
                    *reps = [off as u32, reps[0], reps[1]];
                    off
                }
            };
            if off == 0 || off > pos || len > cap || lcp(d, pos - off, pos, len) < len {
                return Err(corrupt());
            }
            toks.push(Tok::Match { len: len as u16, off: off as u16, rep });
            pos += len;
        }
        Ok(())
    }
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

/// The code lengths block `b` of `parse` ends with: the main tree's and the length tree's.
fn want_lens(parse: &Parse, b: usize) -> ([u8; MAIN], [u8; LEN]) {
    let mut l = lzx::Lens::default();
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
    let mut data = Vec::with_capacity(chunks.len() * CHUNK);
    let mut parses = Vec::with_capacity(chunks.len());
    for (i, &(c, n)) in chunks.iter().enumerate() {
        if n == 0 || n > CHUNK || n < CHUNK && i + 1 < chunks.len() {
            return Err(Error("chunks of another size"));
        }
        if c.len() == n {
            data.extend_from_slice(c);
            parses.push(None);
            continue;
        }
        let (mut d, p) = lzx::read(c, n).ok_or(Error("a chunk not read"))?;
        lzx::e8_decode(&mut d);
        data.extend_from_slice(&d);
        parses.push(Some(p));
    }
    let mut enc = Enc { c: ArithEncoder::new(Vec::new()), cost: [0.0; CATS] };
    let mut w = Walker::new();
    for (raw, p) in data.chunks(CHUNK).zip(&parses) {
        w.chunk(&mut enc, raw, Some(p), None)?;
    }
    let cost = enc.cost;
    Ok(Split { data, record: enc.c.finish(), cost })
}

/// The stream that `data` and `record` rebuild.
pub fn join(data: &[u8], record: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(data.len() / 2);
    join_onto(data, record, &mut out)?;
    Ok(out)
}

/// [`join`] onto `out`.
pub fn join_onto(data: &[u8], record: &[u8], out: &mut Vec<u8>) -> Result<()> {
    let mut dec = Dec { c: ArithDecoder::new(record) };
    let mut w = Walker::new();
    for raw in data.chunks(CHUNK) {
        w.chunk(&mut dec, raw, None, Some(out))?;
        if dec.c.overrun() {
            return Err(corrupt());
        }
    }
    Ok(())
}
