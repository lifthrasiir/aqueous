//! The record of a stream of LZNT1 units given its data (`FORMAT.md` §4), and the stream
//! rebuilt from both.
//!
//! A stream is units one after another: stretches of the data of one length (NTFS's compression
//! units; the last is shorter), or one unit holding a whole buffer. A unit is a hole (no bytes,
//! its data zeros), stored (as long as its data, the data as it is) or compressed: LZNT1 chunks
//! (of 4096 bytes of data), perhaps an end header, and padding to the unit's allocation. The
//! record codes the unit length and the cluster the allocations are multiples of, and for each
//! unit its kind and, for a compressed one:
//!
//! - the chunks' lengths (the standard ones for the unit length, or listed), and for each chunk
//!   whether it is stored and else its tokens, under contexts of what a match finder sees in the
//!   data: of the first two candidates, the longer there (the nearer on ties) and whether one a
//!   byte on is longer; a match is the longer, the other, or else a record-setting candidate or
//!   an offset by its rank among the candidates; the unused flag bits of its last group;
//! - the rest: whether an end header follows, the allocation (against the chunks' bytes rounded
//!   up to the cluster), and the padding where it is not zeros.
//!
//! A compressed unit may hold more data than its unit's (the last unit of a file: Windows
//! compresses the zeros past its end); those are zeros, and are not in the data.
//!
//! Both sides walk a unit alike, so the contexts are the same; the record's coder is
//! aqr-common's binary arithmetic coder, its contexts going on from unit to unit.

use aqr_common::arith::{ArithDecoder, ArithEncoder, Bit};
use aqr_common::lz::lcp_up;

use crate::lznt1::{CHUNK, MIN_MATCH, Tok, Unit, max_len, read_unit, write_chunk, write_stored};
use crate::{Error, Result, corrupt};

/// Links of the chain looked at for the first two candidates.
pub const DEPTH: usize = 16;
/// Candidates looked at for an offset's rank; past them, the offset is written as it is.
pub const LIMIT: usize = 4096;

const NONE: u32 = u32::MAX;
/// Bits of the hash of the match finder's heads.
pub const HBITS: u32 = 13;
const WINDOW: usize = CHUNK;

#[inline]
fn h3(d: &[u8], p: usize) -> usize {
    let k = d[p] as u32 | (d[p + 1] as u32) << 8 | (d[p + 2] as u32) << 16;
    (k.wrapping_mul(0x9e37_79b1) >> (32 - HBITS)) as usize
}

/// The match finder: every position of the unit before the current one on a chain of a hash of
/// its first 3 bytes (their links in a ring of the chunk's size). Positions are those of the
/// unit's data; a chunk's matches stay within it.
struct Mf {
    head: Box<[u32]>,
    prev: Box<[u32]>,
}

impl Mf {
    fn new() -> Mf {
        Mf { head: vec![NONE; 1 << HBITS].into_boxed_slice(), prev: vec![NONE; WINDOW].into_boxed_slice() }
    }

    /// Starts a unit.
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

    /// The first two candidates at `pos` that match 3 bytes or more (at most `cap` of them),
    /// reaching back to `lo`, among the first [`DEPTH`] of the chain: their lengths and offsets,
    /// (0, 0) where none. (Windows' encoder seems to look at no more.)
    fn two(&self, d: &[u8], pos: usize, lo: usize, cap: usize) -> [(usize, usize); 2] {
        let mut out = [(0, 0); 2];
        if cap < MIN_MATCH {
            return out;
        }
        let (mut c, mut n) = (self.chain(d, pos), 0);
        for _ in 0..DEPTH {
            if c == NONE || (c as usize) < lo || n == 2 {
                break;
            }
            let p = c as usize;
            let l = lcp_up(d, p, pos, cap);
            if l >= MIN_MATCH {
                out[n] = (l, pos - p);
                n += 1;
            }
            c = self.prev[p & (WINDOW - 1)];
        }
        out
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
    Rest,
}
pub const CATS: usize = 4;

/// The kinds of a unit, as the contexts index them.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Compressed,
    Stored,
    Hole,
}

struct Ctx {
    compressed: [Bit; 3],
    hole: [Bit; 3],
    std: Bit,
    count: [Bit; 32],
    len: [Bit; 32],
    stored: [Bit; 2],
    slack_nz: Bit,
    slack: [Bit; 32],
    end: [Bit; 2],
    alloc_eq: Bit,
    alloc_by: [Bit; 32],
    pad_nz: Bit,
    /// A token: a match or not (by the longer candidate, the next position's relation, whether
    /// there are two candidates, and the last token's kind).
    is_match: Vec<Bit>,
    /// The candidate a match is at, nearest first (by the nearest one's length and the longer's).
    cand: Vec<[Bit; 32]>,
    /// Whether the match is a record-setting candidate (longer than every one nearer), and which
    /// of them, nearest first.
    is_setter: Vec<Bit>,
    setter: Vec<[Bit; 32]>,
    /// Whether the match is the longer of the first two candidates, or the other (by the length
    /// of the longer).
    ab_win: [Bit; 8],
    ab_other: [Bit; 8],
    /// Whether the offset is none of the candidates, and a length shorter than the match there.
    off_escape: Bit,
    short: Bit,
    short_by: [Bit; 32],
}

/// The vectors of `Ctx` in the order they are made: how many elements, of how many adaptive
/// bits each (a gamma's table has 32).
pub const CTX_VECS: [(usize, usize); 4] = [(8 * 17 * 3 * 2 * 2, 1), (8 * 8, 32), (8 * 8, 1), (8 * 8, 32)];

/// Bytes of what the module holds on WebAssembly (32-bit pointers), checked there by
/// assertions: a stream's data and record and kept bytes, an adaptive bit, a token.
pub mod wasm {
    pub const STREAM: u64 = 24;
    pub const BIT: u64 = 4;
    pub const TOK: u64 = 4;
}

#[cfg(target_family = "wasm")]
const _: () = {
    assert!(size_of::<(&[u8], &[u8], &[u8])>() as u64 == wasm::STREAM);
    assert!(size_of::<Bit>() as u64 == wasm::BIT);
    assert!(size_of::<Tok>() as u64 == wasm::TOK);
};

/// Bytes past the output's length that the module reserves: a chunk written may pass the limit
/// by its size.
pub const SLACK: usize = CHUNK + 16;

impl Ctx {
    fn new() -> Ctx {
        let v = |i: usize| vec![Bit::default(); CTX_VECS[i].0];
        let g = |i: usize| vec![[Bit::default(); 32]; CTX_VECS[i].0];
        Ctx {
            compressed: Default::default(),
            hole: Default::default(),
            std: Bit::default(),
            count: Default::default(),
            len: Default::default(),
            stored: Default::default(),
            slack_nz: Bit::default(),
            slack: Default::default(),
            end: Default::default(),
            alloc_eq: Bit::default(),
            alloc_by: Default::default(),
            pad_nz: Bit::default(),
            is_match: v(0),
            cand: g(1),
            is_setter: v(2),
            setter: g(3),
            ab_win: Default::default(),
            ab_other: Default::default(),
            off_escape: Bit::default(),
            short: Bit::default(),
            short_by: Default::default(),
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

/// What the encoder knows of a compressed unit: its parse, its data (all the chunks', which may
/// pass the unit's), its allocation and the padding.
struct Want<'a> {
    unit: &'a Unit,
    dec: &'a [u8],
    alloc: usize,
    pad: &'a [u8],
}

/// The sampling of a walk to choose its candidates stops with this.
const STOP: Error = Error("sampled");

/// Walks a stream's units over `data`, coding them (`want` given) or decoding them (written
/// onto `out`), as the side is.
struct Walker {
    ctx: Ctx,
    spare: Spare,
    /// The tokens of the chunk.
    toks: Vec<Tok>,
    /// The chunks' lengths of the unit.
    lens: Vec<u16>,
    /// Whether they stay within their capacity (a decoder's).
    fixed: bool,
    /// Whether every position of a match is a candidate for the next (else the tokens' starts).
    all: bool,
    /// The most bytes of output (a decoder's).
    limit: usize,
    prev_kind: usize,
    prev_end: usize,
    /// Bytes of chunks done, and how many to stop at.
    done: usize,
    sample: usize,
}

impl Walker {
    /// A walker on `spare`; with `toks` the chunk's tokens stay within that many, else they grow.
    fn new(spare: Spare, toks: Option<usize>, limit: usize, all: bool) -> Walker {
        Walker {
            ctx: Ctx::new(),
            spare,
            toks: Vec::with_capacity(toks.unwrap_or(0)),
            lens: Vec::new(),
            fixed: toks.is_some(),
            all,
            limit,
            prev_kind: 0,
            prev_end: 0,
            done: 0,
            sample: usize::MAX,
        }
    }

    /// A walker on this thread's spare (else a new one).
    fn parked(toks: Option<usize>, limit: usize, all: bool) -> Walker {
        Walker::new(Spare::parked().unwrap_or_default(), toks, limit, all)
    }

    fn park(self) {
        self.spare.park();
    }
}

impl Walker {
    /// One unit of the stream, `data` its bytes of data: its kind and parse coded (`want`: its
    /// kind and, if compressed, what is known of it) or decoded, the unit written onto `out` when
    /// decoding. `us` is the unit length, `cl` the cluster; `scratch` holds the data padded with
    /// zeros where the chunks have more.
    #[allow(clippy::too_many_arguments)]
    fn unit(
        &mut self,
        side: &mut impl Side,
        us: usize,
        cl: usize,
        data: &[u8],
        want: Option<(Kind, Option<&Want>)>,
        scratch: &mut Vec<u8>,
        mut out: Option<&mut Vec<u8>>,
    ) -> Result<()> {
        let same = Error("a parse the record does not give back");
        let n = data.len();
        let pk = self.prev_kind;
        let wk = want.map(|w| w.0);
        let kind = if side.bit(&mut self.ctx.compressed[pk], wk == Some(Kind::Compressed), Cat::Kind) {
            Kind::Compressed
        } else if side.bit(&mut self.ctx.hole[pk], wk == Some(Kind::Hole), Cat::Kind) {
            Kind::Hole
        } else {
            Kind::Stored
        };
        self.prev_kind = kind as usize;
        match kind {
            Kind::Hole => return Ok(()),
            Kind::Stored => {
                if let Some(o) = out {
                    if o.len() + n > self.limit {
                        return Err(Error("output of another length"));
                    }
                    o.extend_from_slice(data);
                }
                return Ok(());
            }
            Kind::Compressed => {}
        }
        let w = want.and_then(|w| w.1);
        // the chunks' lengths
        let standard = (0..us.div_ceil(CHUNK)).map(|i| CHUNK.min(us - i * CHUNK));
        self.lens.clear();
        let c = &mut self.ctx;
        if side.bit(&mut c.std, w.is_some_and(|w| w.unit.chunks.iter().map(|c| c.len).eq(standard.clone())), Cat::Kind)
        {
            self.lens.extend(standard.map(|l| l as u16));
        } else {
            let count =
                side.gamma(&mut c.count, w.map_or(0, |w| w.unit.chunks.len() as u32 - 1), Cat::Kind)? as usize + 1;
            if count > us {
                return Err(corrupt());
            }
            for i in 0..count {
                let l =
                    side.gamma(&mut c.len, w.map_or(0, |w| w.unit.chunks[i].len as u32 - 1), Cat::Kind)? as usize + 1;
                if l > CHUNK {
                    return Err(corrupt());
                }
                self.lens.push(l as u16);
            }
        }
        let total: usize = self.lens.iter().map(|&l| l as usize).sum();
        if total < n || total > us {
            return Err(corrupt());
        }
        let d: &[u8] = match w {
            Some(w) if w.dec.len() == total => w.dec,
            Some(_) => return Err(same),
            None if total == n => data,
            None => {
                scratch.clear();
                scratch.extend_from_slice(data);
                scratch.resize(total, 0);
                &scratch[..]
            }
        };
        self.spare.mf.reset();
        let o0 = out.as_deref().map_or(0, |o| o.len());
        let (mut base, mut prev_stored) = (0, 0);
        for ci in 0..self.lens.len() {
            let l = self.lens[ci] as usize;
            let cw = w.map(|w| &w.unit.chunks[ci]);
            let stored = side.bit(&mut self.ctx.stored[prev_stored], cw.is_some_and(|c| c.stored), Cat::Kind);
            prev_stored = stored as usize;
            if stored {
                if let Some(o) = out.as_deref_mut() {
                    write_stored(&d[base..base + l], o).ok_or_else(corrupt)?;
                }
            } else {
                self.toks.clear();
                let want_toks = w.zip(cw).map(|(w, c)| &w.unit.toks[c.toks.clone()]);
                self.tokens(side, d, base, base + l, want_toks)?;
                if want_toks.is_some_and(|t| t != &self.toks[..]) {
                    return Err(same);
                }
                let k = self.toks.len() % 8;
                let mut slack = 0;
                if k != 0 {
                    let wv = cw.map_or(0, |c| c.slack as u32);
                    if side.bit(&mut self.ctx.slack_nz, wv != 0, Cat::Kind) {
                        let v = side.gamma(&mut self.ctx.slack, wv.wrapping_sub(1), Cat::Kind)?.wrapping_add(1);
                        if v == 0 || v >> (8 - k) != 0 {
                            return Err(corrupt());
                        }
                        slack = v as u8;
                    }
                }
                if let Some(o) = out.as_deref_mut() {
                    write_chunk(&self.toks, slack, &d[base..base + l], o).ok_or_else(corrupt)?;
                }
            }
            base += l;
            if out.as_deref().is_some_and(|o| o.len() > self.limit) {
                return Err(Error("output of another length"));
            }
            self.done += l;
            if self.done >= self.sample {
                return Err(STOP);
            }
        }
        // the end header and the allocation
        let used = match (&out, w) {
            (Some(o), _) => o.len() - o0,
            (None, Some(w)) => w.unit.used,
            (None, None) => return Err(corrupt()),
        };
        let pe = self.prev_end;
        let end = side.bit(&mut self.ctx.end[pe], w.is_some_and(|w| w.unit.end), Cat::Rest);
        self.prev_end = end as usize;
        let from = used + 2 * end as usize;
        let pred = from.checked_next_multiple_of(cl).ok_or_else(corrupt)?;
        let alloc = if side.bit(&mut self.ctx.alloc_eq, w.is_some_and(|w| w.alloc == pred), Cat::Rest) {
            pred
        } else {
            let by = w.map_or(0, |w| w.alloc.wrapping_sub(from) as u32);
            from.checked_add(side.gamma(&mut self.ctx.alloc_by, by, Cat::Rest)? as usize).ok_or_else(corrupt)?
        };
        let pad = alloc - from;
        if let Some(o) = out.as_deref_mut() {
            if o.len() + alloc - used > self.limit {
                return Err(Error("output of another length"));
            }
            if end {
                o.extend_from_slice(&[0, 0]);
            }
        }
        if pad > 0 {
            let nz = side.bit(&mut self.ctx.pad_nz, w.is_some_and(|w| w.pad.iter().any(|&b| b != 0)), Cat::Rest);
            for i in 0..pad {
                let b = if nz { side.raw(8, w.map_or(0, |w| w.pad[i] as u32), Cat::Rest) as u8 } else { 0 };
                if let Some(o) = out.as_deref_mut() {
                    o.push(b);
                }
            }
        }
        Ok(())
    }
}

impl Walker {
    /// The tokens of a chunk `d[start..end]`, from the start: coded from `want` (all its
    /// tokens) or decoded, onto `self.toks`.
    fn tokens(&mut self, side: &mut impl Side, d: &[u8], start: usize, end: usize, want: Option<&[Tok]>) -> Result<()> {
        let c = &mut self.ctx;
        let mf = &mut *self.spare.mf;
        let (toks, fixed, all) = (&mut self.toks, self.fixed, self.all);
        let (mut pos, mut prev_match, mut t) = (start, 0, 0);
        while pos < end {
            let w = want.map(|w| w.get(t).copied().ok_or_else(corrupt)).transpose()?;
            t += 1;
            if toks.len() == toks.capacity() && fixed {
                return Err(Error("more tokens than a chunk holds"));
            }
            let done = pos - start;
            let cap = (end - pos).min(max_len(done));
            let lo = start;
            let [a, b] = if done == 0 { [(0, 0); 2] } else { mf.two(d, pos, lo, cap) };
            // (the longer of the two, the nearer on ties, and the other)
            let (win, other) = if b.0 > a.0 { (b, a) } else { (a, b) };
            mf.insert(d, pos);
            // (how the next position's longest match compares: none, as long, a byte longer, more)
            let rel = if win.0 >= MIN_MATCH && cap > MIN_MATCH {
                let cap2 = (end - pos - 1).min(max_len(done + 1));
                let [na, nb] = mf.two(d, pos + 1, lo, cap2);
                let next = na.0.max(nb.0);
                if next <= win.0 {
                    0
                } else if next == win.0 + 1 {
                    1
                } else {
                    2
                }
            } else {
                0
            };
            let is_match = if cap < MIN_MATCH || done == 0 {
                if w.is_some_and(|t| t.len != 0) {
                    return Err(corrupt());
                }
                false
            } else {
                let ob = |o: usize| if o == 0 { 0 } else { o.ilog2() as usize + 1 };
                let ctx = (((lb(win.0) * 17 + ob(win.1)) * 3 + rel) * 2 + prev_match) * 2 + (other.0 != 0) as usize;
                side.bit(&mut c.is_match[ctx], w.is_some_and(|t| t.len != 0), Cat::Kind)
            };
            prev_match = is_match as usize;
            if !is_match {
                toks.push(Tok::LIT);
                pos += 1;
                continue;
            }
            let (wlen, woff) = w.map_or((0, 0), |t| (t.len as usize, t.off as usize));
            // the match: the longer of the first two candidates, or the other, else one of the
            // record-setting candidates (longer than every one nearer), by its rank among them,
            // else an offset by its rank among the candidates, or as it is
            let fx = lb(a.0) * 8 + lb(win.0);
            let (mut off, mut len) = (0, 0);
            let mut chosen = false;
            if win.0 != 0 {
                if side.bit(
                    &mut c.ab_win[lb(win.0)],
                    w.is_some_and(|t| (t.len as usize, t.off as usize) == win),
                    Cat::Off,
                ) {
                    (len, off, chosen) = (win.0, win.1, true);
                } else if other.0 != 0
                    && side.bit(
                        &mut c.ab_other[lb(win.0)],
                        w.is_some_and(|t| (t.len as usize, t.off as usize) == other),
                        Cat::Off,
                    )
                {
                    (len, off, chosen) = (other.0, other.1, true);
                }
            }
            let (mut setter, mut found) = (0u32, false);
            if !chosen && want.is_some() && woff != 0 && woff <= pos - lo && wlen == lcp_up(d, pos - woff, pos, cap) {
                mf.setters(d, pos, lo, cap, |o, _| {
                    found = o == woff;
                    setter += 1;
                    o >= woff
                });
                setter -= 1;
            }
            if chosen {
            } else if side.bit(&mut c.is_setter[fx], found, Cat::Off) {
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
                    side.raw(13, woff as u32, Cat::Off) as usize
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
            toks.push(Tok { len: len as u16, off: off as u16 });
            if all {
                for q in pos + 1..pos + len {
                    mf.insert(d, q);
                }
            }
            pos += len;
        }
        Ok(())
    }
}

/// The data and record of a stream of units, each its bytes and its data's length (no bytes: a
/// hole; as many as its data: stored).
pub struct Split {
    pub data: Vec<u8>,
    pub record: Vec<u8>,
    pub cost: [f64; CATS],
}

/// Bytes of the data of a stream of units.
pub fn data_len(units: &[(&[u8], usize)]) -> usize {
    units.iter().map(|c| c.1).sum()
}

/// Splits a stream of units: None where a unit does not read whole, or the units are not all of
/// one length (but the last, which is shorter).
pub fn split(units: &[(&[u8], usize)]) -> Result<Split> {
    let mut data = vec![0; data_len(units)];
    let (us, cl) = read_into(units, &mut data)?;
    if units.is_empty() {
        return Ok(Split { data, record: Vec::new(), cost: [0.0; CATS] });
    }
    let enc = best_walk(&data, units, us, cl)?;
    Ok(Split { data, record: enc.c.finish(), cost: enc.cost })
}

/// [`split`] with the data written into `data` (of [`data_len`] bytes), as where a container
/// holds it all: the record (its cost not counted).
pub fn split_into(units: &[(&[u8], usize)], data: &mut [u8]) -> Result<Vec<u8>> {
    let (us, cl) = read_into(units, data)?;
    if units.is_empty() {
        return Ok(Vec::new());
    }
    Ok(best_walk(data, units, us, cl)?.c.finish())
}

/// The data of a stream of units written into `data` (of [`data_len`] bytes), as [`split_into`]
/// writes it, without the record: the stream's data made again where it was not kept.
pub fn data_into(units: &[(&[u8], usize)], data: &mut [u8]) -> Result<()> {
    read_into(units, data).map(drop)
}

fn gcd(a: usize, b: usize) -> usize {
    if b == 0 { a } else { gcd(b, a % b) }
}

/// The data of the units written into `data`, and the unit length and the cluster of the stream.
fn read_into(units: &[(&[u8], usize)], data: &mut [u8]) -> Result<(usize, usize)> {
    if data.len() != data_len(units) {
        return Err(Error("data of another length"));
    }
    if data.len() >= u32::MAX as usize - WINDOW {
        return Err(Error("data too long"));
    }
    let Some(&(_, first)) = units.first() else { return Ok((1, 1)) };
    let multi = units.len() > 1;
    let (mut dec, mut u) = (Vec::new(), Unit::default());
    let (mut at, mut single, mut g) = (0, None, 0);
    for (i, &(b, n)) in units.iter().enumerate() {
        if n == 0 || multi && (n > first || n < first && i + 1 < units.len()) {
            return Err(Error("units of another length"));
        }
        let out = &mut data[at..at + n];
        at += n;
        if b.is_empty() {
            out.fill(0);
        } else if b.len() == n {
            out.copy_from_slice(b);
        } else {
            read_unit(b, &mut dec, &mut u).ok_or(Error("a unit not read"))?;
            if dec.len() < n || multi && dec.len() > first || dec[n..].iter().any(|&x| x != 0) {
                return Err(Error("a unit of other data"));
            }
            out.copy_from_slice(&dec[..n]);
            g = gcd(g, b.len());
            single = Some(dec.len());
        }
    }
    Ok((if multi { first } else { single.unwrap_or(first) }, g.max(1)))
}

/// Bytes of data that the choice of [`walk`]'s candidates is made on.
const SAMPLE: usize = 256 << 10;

/// The record of `data` coded with the candidates the stream's encoder seems to use: tried on its
/// first units (up to [`SAMPLE`] bytes), the cheaper going on to the rest.
fn best_walk(data: &[u8], units: &[(&[u8], usize)], us: usize, cl: usize) -> Result<Enc> {
    let run = |all: bool, sample: usize| {
        let mut enc = Enc::with(ArithEncoder::new(Vec::new()));
        match walk(&mut enc, data, units, us, cl, all, sample) {
            Ok(()) => Ok(enc),
            Err(e) if e == STOP => Ok(enc),
            Err(e) => Err(e),
        }
    };
    let small = data.len() <= SAMPLE;
    let sample = if small { usize::MAX } else { SAMPLE };
    let (a, b) = (run(false, sample)?, run(true, sample)?);
    let all = b.cost.iter().sum::<f64>() < a.cost.iter().sum::<f64>();
    if small {
        // (all of it was coded)
        return Ok(if all { b } else { a });
    }
    run(all, usize::MAX)
}

/// The record of the units of `data` coded onto `enc`, as far as `sample` bytes of chunks.
fn walk(
    enc: &mut impl Side,
    data: &[u8],
    units: &[(&[u8], usize)],
    us: usize,
    cl: usize,
    all: bool,
    sample: usize,
) -> Result<()> {
    let mut g = [Bit::default(); 32];
    enc.gamma(&mut g, us as u32, Cat::Rest)?;
    enc.gamma(&mut g, cl as u32, Cat::Rest)?;
    enc.bit(&mut Bit::default(), all, Cat::Rest);
    let mut w = Walker::parked(None, usize::MAX, all);
    w.sample = sample;
    let (mut dec, mut u, mut scratch) = (Vec::new(), Unit::default(), Vec::new());
    let done = (|| {
        let mut at = 0;
        for &(b, n) in units {
            let d = &data[at..at + n];
            at += n;
            if b.is_empty() {
                w.unit(enc, us, cl, d, Some((Kind::Hole, None)), &mut scratch, None)?;
            } else if b.len() == n {
                w.unit(enc, us, cl, d, Some((Kind::Stored, None)), &mut scratch, None)?;
            } else {
                read_unit(b, &mut dec, &mut u).ok_or(Error("a unit not read"))?;
                let pad = &b[u.used + 2 * u.end as usize..];
                let want = Want { unit: &u, dec: &dec, alloc: b.len(), pad };
                w.unit(enc, us, cl, d, Some((Kind::Compressed, Some(&want))), &mut scratch, None)?;
            }
        }
        Ok(())
    })();
    w.park();
    done
}

/// The most tokens a chunk holds, in a stream of units of `us` bytes.
pub fn block_toks(us: usize) -> usize {
    us.min(CHUNK)
}

/// The stream that `data` and `record` rebuild.
pub fn join(data: &[u8], record: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(data.len() / 2 + 64);
    join_onto(data, record, &mut out)?;
    Ok(out)
}

/// [`join`] onto `out`.
pub fn join_onto(data: &[u8], record: &[u8], out: &mut Vec<u8>) -> Result<()> {
    // (a damaged record has no unit padded by more than a megabyte, nor a stream twice its data)
    let limit = out.len().saturating_add(data.len().saturating_mul(2)).saturating_add(1 << 20);
    let (spare, done) = join_in(Spare::parked().unwrap_or_default(), data, record, out, limit);
    spare.park();
    done
}

/// The unit length and the cluster that `record` starts with, of a stream of `n` bytes of data,
/// and whether the candidates are all the positions.
pub fn read_head(dec: &mut ArithDecoder, n: usize) -> Result<(usize, usize, bool)> {
    let mut g = [Bit::default(); 32];
    let us = dec.gamma(&mut g, 0, Cat::Rest)? as usize;
    let cl = dec.gamma(&mut g, 0, Cat::Rest)? as usize;
    if us == 0 || cl == 0 || us > n.max(MAX_UNIT) {
        return Err(corrupt());
    }
    Ok((us, cl, dec.bit(&mut Bit::default(), false, Cat::Rest)))
}

/// The longest unit that is longer than the stream's data.
pub const MAX_UNIT: usize = 1 << 20;

/// The stream that `data` and `record` rebuild, onto `out` (room for `limit` bytes, and
/// [`SLACK`] more, reserved), on `spare`: the spare back, to go on with.
pub fn join_in(spare: Spare, data: &[u8], record: &[u8], out: &mut Vec<u8>, limit: usize) -> (Spare, Result<()>) {
    if data.is_empty() {
        return (spare, if record.is_empty() { Ok(()) } else { Err(corrupt()) });
    }
    let mut dec = ArithDecoder::new(record);
    let (us, cl, all) = match read_head(&mut dec, data.len()) {
        Ok(s) => s,
        Err(e) => return (spare, Err(e)),
    };
    let mut w = Walker::new(spare, Some(block_toks(us)), limit, all);
    let mut scratch = Vec::new();
    if !data.len().is_multiple_of(us) {
        scratch.reserve_exact(us);
    }
    let done = (|| {
        if data.len() >= u32::MAX as usize - WINDOW {
            return Err(corrupt());
        }
        for i in 0..data.len().div_ceil(us) {
            let d = &data[i * us..][..us.min(data.len() - i * us)];
            w.unit(&mut dec, us, cl, d, None, &mut scratch, Some(out))?;
            if dec.overrun() {
                return Err(corrupt());
            }
        }
        Ok(())
    })();
    (w.spare, done)
}
