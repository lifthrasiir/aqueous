//! The record of an LZMA stream's parse given its data.
//!
//! A match's distance is not coded where it is the nearest one matching for its length (what a
//! binary-tree match finder offers an optimal parser). Lengths are coded against the longest
//! match there, and packet types under contexts of what the data offers at the position. Both
//! sides see the same data, so the contexts are the same.

use aqr_common::varint;

use crate::arith::{ArithDecoder, ArithEncoder, Bit};
use crate::lzma::{self, Kind, MAX_LEN, MIN_LEN, Packet, Props, StreamEncoder};
use crate::lzma2::{CHUNK_CODED, CHUNK_DATA, Chunk};
use crate::mf::{DEPTH, Mf};
use crate::stream::{Body, Pull, Put, Reader};
use crate::window::{View, Window};
use crate::{Result, corrupt};

/// A match this long ends the search for the longest.
const NICE: usize = 64;
/// Candidates looked at for the nearest match of a length beyond those: a distance not found
/// is coded instead.
const LIMIT: usize = 64;

#[inline]
fn bucket(x: usize) -> usize {
    ((usize::BITS - x.leading_zeros()) as usize).min(7)
}

/// What the record codes, for the statistics.
#[derive(Clone, Copy)]
#[repr(usize)]
pub enum Cat {
    Type,
    Len,
    Dist,
    RepLen,
    /// An LZMA2 chunk's header.
    Head,
}
pub const CATS: usize = 5;

struct Tree9([Bit; 512]);

impl Default for Tree9 {
    fn default() -> Self {
        Tree9([Bit::default(); 512])
    }
}

/// Bits of the contexts held in one allocation (`Ctx`): the lengths of the lists of
/// probabilities, in order.
pub const CTX_LENS: [usize; 14] =
    [12 * 64, 12 * 64, 12 * 64, 12 * 8, 64, 64, 8 * 16, 4 * 8, 8 * 512, 8 * 512, 64, 7 * 8, 32, 256];
/// The bits of all of them.
pub const CTX_BITS: usize = {
    let (mut i, mut n) = (0, 0);
    while i < CTX_LENS.len() {
        n += CTX_LENS[i];
        i += 1;
    }
    n
};

/// Bytes of what the module holds on WebAssembly (32-bit pointers), checked there by assertions:
/// a context's bit, a part with its body, a packet's candidate (length, position).
pub mod wasm {
    pub const BIT: u64 = 4;
    pub const PART: u64 = 32;
    pub const PAIR: u64 = 8;
}

#[cfg(target_family = "wasm")]
const _: () = {
    assert!(size_of::<Bit>() as u64 == wasm::BIT);
    assert!(size_of::<(Part, &[u8])>() as u64 == wasm::PART);
    assert!(size_of::<(u32, u32)>() as u64 == wasm::PAIR);
};

/// The matches [`Mf::pairs`] gives at most: those found on each of two chains, and one for each
/// of the 2- and 3-byte keys.
pub const PAIRS: usize = 2 * DEPTH + 2;

struct Ctx<'a> {
    is_match: &'a mut [Bit],
    is_rep: &'a mut [Bit],
    g0: &'a mut [Bit],
    rep0_long: &'a mut [Bit],
    g1: &'a mut [Bit],
    g2: &'a mut [Bit],
    /// A match: which pair (from the longest), whether at its length, else how much shorter.
    pair: &'a mut [Bit],
    pair_more: Tree9,
    pair_full: &'a mut [Bit],
    /// Eight trees of 512.
    pair_short: &'a mut [Bit],
    match_len: Tree9,
    far: Bit,
    /// A distance not found: its offset from the nearest of the last four.
    far_rep: [Bit; 4],
    far_sign: [Bit; 4],
    far_bucket: &'a mut [Bit],
    rep_full: [[Bit; 8]; 4],
    /// Eight trees of 512.
    rep_len: &'a mut [Bit],
    /// An LZMA2 chunk's control (by the last one's), the length of its data and its properties.
    chunk_kind: &'a mut [Bit],
    chunk_len: &'a mut [Bit],
    chunk_props: &'a mut [Bit],
}

impl<'a> Ctx<'a> {
    /// Over `store` ([`CTX_BITS`] of them, all at their start).
    fn new(store: &'a mut [Bit]) -> Self {
        let mut rest = store;
        let mut take = |n: usize| {
            let (a, b) = std::mem::take(&mut rest).split_at_mut(n);
            rest = b;
            a
        };
        let [a, b, c, d, e, f, g, h, i, j, k, l, m, n] = CTX_LENS.map(&mut take);
        Ctx {
            is_match: a,
            is_rep: b,
            g0: c,
            rep0_long: d,
            g1: e,
            g2: f,
            pair: g,
            pair_more: Tree9::default(),
            pair_full: h,
            pair_short: i,
            match_len: Tree9::default(),
            far: Bit::default(),
            far_rep: [Bit::default(); 4],
            far_sign: [Bit::default(); 4],
            far_bucket: k,
            rep_full: [[Bit::default(); 8]; 4],
            rep_len: j,
            chunk_kind: l,
            chunk_len: m,
            chunk_props: n,
        }
    }
}

/// The side of the record's coder, as [`crate::rc::Bits`] for LZMA's.
pub(crate) trait Side {
    fn bit(&mut self, b: &mut Bit, v: bool, cat: Cat) -> bool;
    /// `n` bits of probability 1/2.
    fn raw(&mut self, n: u32, v: u32, cat: Cat) -> u32;
    /// `n` bits under the contexts of the bits before them.
    fn tree(&mut self, t: &mut [Bit], n: u32, v: u32, cat: Cat) -> u32 {
        let mut m = 1;
        for i in (0..n).rev() {
            m = m << 1 | self.bit(&mut t[m], v >> i & 1 != 0, cat) as usize;
        }
        m as u32 - (1 << n)
    }

    /// `v` as `x = v + 1` (wrapping): the bit length of `x` less 1 in unary (adaptive bits of
    /// `t`, the last one for the lengths beyond), then the bits of `x` below its top one, raw.
    /// None where the unary runs past 32 bits.
    fn gamma(&mut self, t: &mut [Bit], v: u32, cat: Cat) -> Option<u32> {
        let x = v.wrapping_add(1);
        let n = 31 - x.max(1).leading_zeros();
        let mut k = 0;
        while self.bit(&mut t[(k as usize).min(t.len() - 1)], k < n, cat) {
            k += 1;
            if k > 31 {
                return None;
            }
        }
        let y = 1u32 << k | self.raw(k, x & ((1 << k) - 1), cat);
        Some(y - 1)
    }

    fn tree9(&mut self, t: &mut [Bit], v: u32, cat: Cat) -> u32 {
        let t = &mut t[..512];
        let mut m = 1;
        for i in (0..9).rev() {
            m = m << 1 | self.bit(&mut t[m], v >> i & 1 != 0, cat) as usize;
        }
        m as u32 - 512
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

/// Where the packets of a part of a record start ([`split_with`]), and the state and the last
/// distances there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Part {
    pub at: usize,
    pub state: u8,
    pub reps: [u32; 4],
}

impl Part {
    const START: Part = Part { at: 0, state: 0, reps: [0; 4] };
}

/// The state and the last distances after packet `pk`, from those before it.
fn step(state: &mut u8, reps: &mut [u32; 4], pk: Packet) {
    let s = *state;
    *state = match pk.kind {
        Kind::Lit if s < 4 => 0,
        Kind::Lit if s < 10 => s - 3,
        Kind::Lit => s - 6,
        Kind::ShortRep => {
            if s < 7 {
                9
            } else {
                11
            }
        }
        Kind::Match => {
            if s < 7 {
                7
            } else {
                10
            }
        }
        _ => {
            if s < 7 {
                8
            } else {
                11
            }
        }
    };
    let k = match pk.kind {
        Kind::Lit | Kind::ShortRep => return,
        Kind::Match => 3,
        Kind::Rep0 => 0,
        Kind::Rep1 => 1,
        Kind::Rep2 => 2,
        Kind::Rep3 => 3,
    };
    let d = if pk.kind == Kind::Match { pk.dist } else { reps[k] };
    reps.copy_within(0..k, 1);
    reps[0] = d;
}

/// What a walk gives: the header of an LZMA2 chunk, where it starts, and the packets in it.
#[derive(Clone, Copy)]
pub enum Ev {
    Chunk(Chunk),
    Packet(Packet),
}

/// The chunks of an LZMA2 segment: not there (LZMA), coded in the record, given to code them, or
/// decoded from it. A walk of chunks goes over all the data. A chunk's header is coded where
/// the walk reaches its start; the packets in it end by its end.
#[derive(Clone, Copy)]
pub enum Chunks<'a> {
    Off,
    Encode(&'a [Chunk]),
    Decode,
}

/// The controls of the chunks: by the index the header codes.
const CONTROLS: [u8; 6] = [1, 2, 0x80, 0xA0, 0xC0, 0xE0];

/// A chunk's header, given (coding) or not (decoding) and the chunk that came before it (the
/// index of its control, or [`CONTROLS`]'s length for none): the header and the index of its
/// control. None where it is no chunk of LZMA2.
fn code_chunk<S: Side>(s: &mut S, x: &mut Ctx, prev: usize, given: Option<Chunk>) -> Option<(Chunk, usize)> {
    let kind = given.and_then(|c| CONTROLS.iter().position(|&k| k == c.control)).unwrap_or(0);
    let kind = s.tree(&mut x.chunk_kind[prev * 8..], 3, kind as u32, Cat::Head) as usize;
    let control = *CONTROLS.get(kind)?;
    let len = s.gamma(x.chunk_len, given.map_or(0, |c| c.len as u32).wrapping_sub(1), Cat::Head)?;
    let props = match control {
        0xC0.. => {
            let b = given.and_then(|c| c.props).map_or(0, |p| p.byte() as u32);
            let b = s.tree(x.chunk_props, 8, b, Cat::Head);
            Some(Props::from_byte(b as u8).filter(|p| p.lc + p.lp <= 4)?)
        }
        _ => None,
    };
    Some((Chunk { control, len: (len as usize).checked_add(1)?, props }, kind))
}

/// Walks the packets of the data of `win` from `part` to `end` (a packet's start, or the end of
/// the data) with matches at most `reach` back, under contexts of their own and with `mf`
/// (holding the positions within reach before `part`), coding them (`Some`) or decoding them
/// (`None`, until `end` is reached), and gives each to `on` (with the data it looks at in
/// view), after the header of the chunk that holds it. None where the record does not fit the
/// data, or a packet goes past a chunk or `end`, or `on` fails.
///
/// In a module it is inlined into its one caller, which then drops the chunks it never meets
/// (the heuristic flipped once a header byte was added: 3.4 KB more for lzma-recons).
#[allow(clippy::too_many_arguments)]
#[cfg_attr(target_arch = "wasm32", inline(always))]
fn walk<S: Side>(
    s: &mut S,
    mf: &mut Mf,
    win: &mut Window,
    reach: usize,
    chunks: Chunks,
    part: Part,
    end: usize,
    mut given: Option<&[Packet]>,
    mut on: impl FnMut(&View, Ev) -> Option<()>,
) -> Option<()> {
    let total = win.total();
    if !matches!(chunks, Chunks::Off) && (part.at != 0 || end != total) {
        return None;
    }
    // (a last distance is within reach: the window holds no more)
    if part.reps.iter().any(|&r| r as usize >= reach) {
        return None;
    }
    // (the contexts, then the pairs: dropped in the reverse order)
    let mut store = vec![Bit::default(); CTX_BITS];
    let mut x = Ctx::new(&mut store);
    let (mut state, mut reps) = (part.state as usize, part.reps);
    let mut pairs = Vec::with_capacity(PAIRS);
    let mut pos = part.at;
    let (mut i, mut ci, mut prev, mut chunk_end) = (0, 0, CONTROLS.len(), 0);
    loop {
        win.ensure(pos)?;
        if !matches!(chunks, Chunks::Off) {
            // (a chunk's header where the last one ends)
            while pos == chunk_end && pos < total {
                let want = match chunks {
                    Chunks::Encode(c) => Some(*c.get(ci)?),
                    _ => None,
                };
                let (c, kind) = code_chunk(s, &mut x, prev, want)?;
                let max = if c.stored() { CHUNK_CODED } else { CHUNK_DATA };
                if want.is_some_and(|g| g != c) || (ci == 0) != c.resets_dict() || c.len > max || c.len > total - pos {
                    return None;
                }
                on(&win.view(), Ev::Chunk(c))?;
                (ci, prev) = (ci + 1, kind);
                if c.control >= 0xA0 {
                    (state, reps) = (0, [0; 4]);
                }
                if c.stored() {
                    pos += c.len;
                    chunk_end = pos;
                    win.ensure(pos)?;
                } else {
                    chunk_end = pos + c.len;
                }
            }
            if pos > chunk_end {
                return None;
            }
        }
        if pos >= end {
            break;
        }
        let v = win.view();
        // (packets end at the next chunk's start)
        let lim = if matches!(chunks, Chunks::Off) { total } else { chunk_end };
        let pk = match &mut given {
            Some(g) => *g.get(i)?,
            None => Packet::LIT,
        };
        i += 1;
        mf.advance(&v, pos);
        let cap = (MAX_LEN as usize).min(lim - pos);
        let mut rl = [0usize; 4];
        for k in 0..4 {
            if (reps[k] as usize) < pos.min(reach) {
                rl[k] = v.lcp(pos - reps[k] as usize - 1, pos, cap);
            }
        }
        let hint = mf.first(&v, pos, cap);
        let (bm, b0) = (bucket(hint), bucket(rl[0]));
        let brest = bucket(rl[1].max(rl[2]).max(rl[3]));
        let ctx = state * 64 + bm * 8 + b0;
        if !s.bit(&mut x.is_match[ctx], pk.kind != Kind::Lit, Cat::Type) {
            state = if state < 4 {
                0
            } else if state < 10 {
                state - 3
            } else {
                state - 6
            };
            on(&v, Ev::Packet(Packet::LIT))?;
            pos += 1;
            continue;
        }
        let pk = if s.bit(
            &mut x.is_rep[state * 64 + bm * 8 + bucket(rl[0].max(rl[1]).max(rl[2]).max(rl[3]))],
            pk.kind != Kind::Match,
            Cat::Type,
        ) {
            let k = match pk.kind {
                Kind::Rep1 => 1,
                Kind::Rep2 => 2,
                Kind::Rep3 => 3,
                _ => 0,
            };
            let k = if !s.bit(&mut x.g0[state * 64 + b0 * 8 + brest], k != 0, Cat::Type) {
                if !s.bit(&mut x.rep0_long[state * 8 + b0], pk.kind != Kind::ShortRep, Cat::Type) {
                    if rl[0] == 0 {
                        return None;
                    }
                    state = if state < 7 { 9 } else { 11 };
                    on(&v, Ev::Packet(Packet::SHORT_REP))?;
                    pos += 1;
                    continue;
                }
                0
            } else if !s.bit(&mut x.g1[bucket(rl[1]) * 8 + bucket(rl[2].max(rl[3]))], k != 1, Cat::Type) {
                1
            } else if !s.bit(&mut x.g2[bucket(rl[2]) * 8 + bucket(rl[3])], k == 3, Cat::Type) {
                2
            } else {
                3
            };
            let l = rl[k];
            let n = if s.bit(&mut x.rep_full[k][bucket(l)], pk.len as usize == l, Cat::RepLen) {
                l
            } else {
                s.tree9(&mut x.rep_len[bucket(l) * 512..], (pk.len as u32).wrapping_sub(MIN_LEN), Cat::RepLen) as usize
                    + MIN_LEN as usize
            };
            if n < MIN_LEN as usize || n > l {
                return None;
            }
            let d = reps[k];
            reps.copy_within(0..k, 1);
            reps[0] = d;
            state = if state < 7 { 8 } else { 11 };
            Packet { kind: [Kind::Rep0, Kind::Rep1, Kind::Rep2, Kind::Rep3][k], len: n as u16, dist: d }
        } else {
            mf.longest(&v, pos, DEPTH, NICE, cap);
            mf.pairs(&v, pos, cap, &mut pairs);
            let m = pairs.len();
            let j = pairs.iter().position(|&(l, _)| l as usize >= pk.len as usize).map_or(m, |i| m - 1 - i);
            let mut jj = s.tree(&mut x.pair[m.min(7) * 16..][..16], 4, j.min(15) as u32, Cat::Len) as usize;
            if jj == 15 {
                jj += s.tree9(&mut x.pair_more.0, j.wrapping_sub(15) as u32, Cat::Len) as usize;
            }
            let n = if jj >= m {
                // Longer than any found.
                let top = pairs.last().map_or(MIN_LEN as usize - 1, |&(l, _)| l as usize);
                let n = s.tree9(&mut x.match_len.0, (pk.len as u32).wrapping_sub(MIN_LEN), Cat::Len) as usize
                    + MIN_LEN as usize;
                if jj > m || n <= top {
                    return None;
                }
                n
            } else {
                let i = m - 1 - jj;
                let top = pairs[i].0 as usize;
                let lo = if i > 0 { pairs[i - 1].0 as usize + 1 } else { MIN_LEN as usize };
                if s.bit(&mut x.pair_full[jj.min(3) * 8 + bucket(top)], pk.len as usize == top, Cat::Len) {
                    top
                } else {
                    let v = s.tree9(
                        &mut x.pair_short[bucket(top) * 512..],
                        (top as u32).wrapping_sub(pk.len as u32 + 1),
                        Cat::Len,
                    );
                    let n = (top as u32).wrapping_sub(v + 1) as usize;
                    if n < lo || n >= top {
                        return None;
                    }
                    n
                }
            };
            if n < MIN_LEN as usize || n > cap {
                return None;
            }
            let near = mf.nearest(&v, pos, n, LIMIT);
            let is_near = near.is_some_and(|q| pk.dist as usize == pos - q - 1);
            let d = if s.bit(&mut x.far, !is_near, Cat::Dist) {
                // The last distance nearest to it, the sign, and the offset by its bit length.
                let off = |k: usize| pk.dist as i64 - reps[k] as i64;
                let k = (0..4).min_by_key(|&k| off(k).unsigned_abs()).unwrap();
                let k = s.tree(&mut x.far_rep, 2, k as u32, Cat::Dist) as usize;
                let neg = s.bit(&mut x.far_sign[k], off(k) < 0, Cat::Dist);
                let m = off(k).unsigned_abs() as u32;
                let b = s.tree(x.far_bucket, 6, 32 - m.leading_zeros(), Cat::Dist);
                if b > 32 {
                    return None;
                }
                let m = if b <= 1 { b } else { 1 << (b - 1) | s.raw(b - 1, m, Cat::Dist) };
                let d = reps[k] as i64 + if neg { -(m as i64) } else { m as i64 };
                if !(0..pos.min(reach) as i64).contains(&d) {
                    return None;
                }
                d as u32
            } else {
                (pos - near? - 1) as u32
            };
            if d as usize >= pos.min(reach) || v.lcp(pos - d as usize - 1, pos, n) < n {
                return None;
            }
            reps.copy_within(0..3, 1);
            reps[0] = d;
            state = if state < 7 { 7 } else { 10 };
            Packet { kind: Kind::Match, len: n as u16, dist: d }
        };
        on(&v, Ev::Packet(pk))?;
        pos += pk.len as usize;
    }
    let all = match chunks {
        Chunks::Encode(c) => ci == c.len(),
        _ => true,
    };
    (pos == end && all && given.is_none_or(|g| g.len() == i)).then_some(())
}

/// [`walk`] over all the data of `win`, decoding the packets and chunks of an LZMA2 segment from
/// `s`.
pub(crate) fn walk_chunks<S: Side>(
    s: &mut S,
    mf: &mut Mf,
    win: &mut Window,
    reach: usize,
    on: impl FnMut(&View, Ev) -> Option<()>,
) -> Option<()> {
    let n = win.total();
    walk(s, mf, win, reach, Chunks::Decode, Part::START, n, None, on)
}

/// The record of the packets of `data` (from [`crate::lzma::decode`]) for a dictionary of
/// `dict` bytes and a match finder of the reach `fin` ([`finder_of`]), with the `chunks` of an
/// LZMA2 segment (none for LZMA), and its cost by [`Cat`] in bits. None where the packets do not
/// cover the data exactly, or `fin` is not one.
pub fn deconstruct(
    data: &[u8],
    packets: &[Packet],
    dict: u32,
    fin: u8,
    chunks: Option<&[Chunk]>,
) -> Option<(Vec<u8>, [f64; CATS])> {
    let mut e = Enc::with(ArithEncoder::new(vec![]));
    let mut mf = Mf::new(data.len(), finder_of(dict, fin)?);
    let chunks = chunks.map_or(Chunks::Off, Chunks::Encode);
    let mut win = Window::whole(data);
    walk(&mut e, &mut mf, &mut win, reach(dict), chunks, Part::START, data.len(), Some(packets), |_, _| Some(()))?;
    Some((e.c.finish(), e.cost))
}

/// The packets of `data` from its record, given to `on` as they come, the chunks of an LZMA2
/// segment (`chunks`) too. None where the record does not fit the data (some packets may have
/// been given by then). `fin` is the reach of the match finder, as in [`deconstruct`].
pub fn reconstruct(
    data: &[u8],
    record: &[u8],
    dict: u32,
    fin: u8,
    chunks: bool,
    on: impl FnMut(&View, Ev) -> Option<()>,
) -> Option<()> {
    let mut d = ArithDecoder::new(record);
    let mut mf = Mf::new(data.len(), finder_of(dict, fin)?);
    let chunks = if chunks { Chunks::Decode } else { Chunks::Off };
    walk(&mut d, &mut mf, &mut Window::whole(data), reach(dict), chunks, Part::START, data.len(), None, on)?;
    (!d.overrun()).then_some(())
}

/// Bytes of data from which a record is cut into parts, at the first packet at or past each
/// multiple, for their packets to be coded and decoded side by side ([`split`]).
pub const PART: usize = 64 << 20;

/// The parts of the packets of `n` bytes of data, `part` bytes apart (at the first packet at or
/// past each multiple), with the index of the first packet of each.
fn parts_of(packets: &[Packet], part: usize) -> Vec<(Part, usize)> {
    let mut out = vec![(Part::START, 0)];
    let (mut pos, mut state, mut reps, mut next) = (0usize, 0u8, [0u32; 4], part);
    for (i, &pk) in packets.iter().enumerate() {
        if pos >= next {
            out.push((Part { at: pos, state, reps }, i));
            next = (pos / part + 1).saturating_mul(part);
        }
        step(&mut state, &mut reps, pk);
        pos += pk.len as usize;
    }
    out
}

/// The body of the record of `packets` of `data` cut into `parts` (at least two; see FORMAT.md,
/// "Parts"), each coded on its own: side by side on up to `threads` threads where more than one
/// match finder works at once ([`Mf::at_once`]), else in turn with one.
fn deconstruct_parts(
    data: &[u8],
    packets: &[Packet],
    dict: u32,
    fin: u8,
    parts: &[(Part, usize)],
    threads: usize,
) -> Option<Vec<u8>> {
    let reach = reach(dict);
    let finder = finder_of(dict, fin)?;
    let code = |k: usize, mf: &mut Mf| {
        let (part, first) = parts[k];
        let (end, last) = parts.get(k + 1).map_or((data.len(), packets.len()), |&(p, i)| (p.at, i));
        let mut e = Enc::with(ArithEncoder::new(vec![]));
        let mut win = Window::whole(data);
        walk(&mut e, mf, &mut win, reach, Chunks::Off, part, end, Some(&packets[first..last]), |_, _| Some(()))?;
        Some(e.c.finish())
    };
    let bodies: Vec<Option<Vec<u8>>> = if threads <= 1 || Mf::at_once(data.len(), finder) < 2 {
        let mut mf = Mf::new(data.len(), finder);
        (0..parts.len()).map(|k| code(k, &mut mf)).collect()
    } else {
        aqr_common::pool::par_map(parts.len(), threads, |k| {
            code(k, &mut Mf::starting(data.len(), finder, parts[k].0.at.saturating_sub(finder)))
        })
    };
    let bodies: Vec<Vec<u8>> = bodies.into_iter().collect::<Option<_>>()?;
    let mut out = Vec::new();
    varint::put(&mut out, parts.len() as u64 - 1);
    for (k, body) in bodies.iter().enumerate() {
        let (p, _) = parts[k];
        let end = parts.get(k + 1).map_or(data.len(), |q| q.0.at);
        varint::put(&mut out, (end - p.at) as u64);
        varint::put(&mut out, body.len() as u64);
        if k > 0 {
            out.push(p.state);
            p.reps.iter().for_each(|&r| varint::put(&mut out, r as u64));
        }
        out.extend_from_slice(body);
    }
    Some(out)
}

/// The number of parts of a body of a record cut into them (see [`deconstruct_parts`]) for data
/// of `n` bytes: the first varint, and one; None where it is not a count of them. Each part
/// after the first has seven bytes of its header at least.
fn parts_count(body: &[u8], n: usize) -> Option<usize> {
    let k =
        usize::try_from(varint::get(body, &mut 0).ok()?).ok().filter(|&k| k >= 1 && k < n && k <= body.len() / 7)?;
    Some(k + 1)
}

/// A part of a record cut into them: where its packets start and end, and its body.
struct PartRec<'a> {
    part: Part,
    end: usize,
    body: &'a [u8],
}

/// The parts of a body of a record cut into them (see [`deconstruct_parts`]) of data of `n`
/// bytes, a dictionary of `reach`: None where it is not one.
fn parts_in(body: &[u8], n: usize, reach: usize) -> Option<Vec<PartRec<'_>>> {
    let mut at = 0;
    let get = |at: &mut usize| varint::get(body, at).ok();
    let count = parts_count(body, n)?;
    get(&mut at)?;
    let mut parts = Vec::with_capacity(count);
    let mut pos = 0usize;
    for k in 0..count {
        let span = usize::try_from(get(&mut at)?).ok().filter(|&s| s > 0 && s <= n - pos)?;
        let len = usize::try_from(get(&mut at)?).ok()?;
        let (mut state, mut reps) = (0u8, [0u32; 4]);
        if k > 0 {
            state = *body.get(at).filter(|&&s| s < 12)?;
            at += 1;
            for r in &mut reps {
                *r = u32::try_from(get(&mut at)?).ok().filter(|&r| (r as usize) < reach)?;
            }
        }
        let b = body.get(at..)?.get(..len)?;
        at += len;
        parts.push(PartRec { part: Part { at: pos, state, reps }, end: pos + span, body: b });
        pos += span;
    }
    (pos == n && at == body.len()).then_some(parts)
}

/// The stream of `data` from the `parts` of its record, on up to `threads` threads where more
/// than one match finder works at once ([`Mf::at_once`]: the parts' packets decoded side by
/// side, then encoded in order), with the end marker if `marker`; else one goes through the
/// parts with one match finder. Written onto the end of `out`.
#[allow(clippy::too_many_arguments)]
fn rebuild_parts(
    data: &[u8],
    parts: &[PartRec],
    props: Props,
    dict: u32,
    fin: u8,
    marker: bool,
    threads: usize,
    out: &mut Vec<u8>,
) -> Option<()> {
    let reach = reach(dict);
    let finder = finder_of(dict, fin)?;
    let whole = View::whole(data);
    let mut e = StreamEncoder::new(props, std::mem::take(out));
    if threads <= 1 || parts.len() == 1 || Mf::at_once(data.len(), finder) < 2 {
        let mut mf = Mf::new(data.len(), finder);
        for p in parts {
            let mut d = ArithDecoder::new(p.body);
            let on = |v: &View, ev| {
                if let Ev::Packet(pk) = ev {
                    e.push(v, pk);
                }
                Some(())
            };
            walk(&mut d, &mut mf, &mut Window::whole(data), reach, Chunks::Off, p.part, p.end, None, on)?;
            if d.overrun() {
                return None;
            }
        }
    } else {
        let packets = aqr_common::pool::par_map(parts.len(), threads, |k| {
            let p = &parts[k];
            let mut d = ArithDecoder::new(p.body);
            let mut mf = Mf::starting(data.len(), finder, p.part.at.saturating_sub(finder));
            let mut out = Vec::new();
            let on = |_: &View, ev| {
                if let Ev::Packet(pk) = ev {
                    out.push(pk);
                }
                Some(())
            };
            walk(&mut d, &mut mf, &mut Window::whole(data), reach, Chunks::Off, p.part, p.end, None, on)?;
            (!d.overrun()).then_some(out)
        });
        for p in packets {
            p?.into_iter().for_each(|pk| e.push(&whole, pk));
        }
    }
    *out = e.finish(&whole, marker);
    Some(())
}

/// The stream of `data` from its record, with the end marker if `marker`.
pub fn rebuild(data: &[u8], record: &[u8], props: Props, dict: u32, fin: u8, marker: bool) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(data.len() / 4);
    let part = [PartRec { part: Part::START, end: data.len(), body: record }];
    rebuild_parts(data, &part, props, dict, fin, marker, 1, &mut out)?;
    Some(out)
}

/// How far back a match may be: decoders keep at least 4 KiB.
pub fn reach(dict: u32) -> usize {
    dict.max(1 << 12) as usize
}

/// The reach of the match finder that a record's byte `fin` tells, for a dictionary of `dict`
/// bytes: 0 for the dictionary's ([`reach`]), else `2^fin` (12 to 31) if that is less. The
/// finder holds the positions within it only; the window of the data, and the distances a
/// packet may have, stay the dictionary's.
pub fn finder_of(dict: u32, fin: u8) -> Option<usize> {
    let reach = reach(dict);
    match fin {
        0 => Some(reach),
        12..=31 if (1usize << fin) < reach => Some(1usize << fin),
        _ => None,
    }
}

/// The bytes of a record before its body: the properties byte, the dictionary size (le32), the
/// flags (bit 0: the stream ends with an end marker; bit 1: the body is cut into parts) and the
/// match finder's reach ([`finder_of`]).
pub const HEADER: usize = 7;

/// A stream split into its data and its record.
#[derive(Clone)]
pub struct Split {
    pub data: Vec<u8>,
    pub record: Vec<u8>,
    /// The bytes of the input the stream took (what follows is not the stream's).
    pub used: usize,
}

/// Splits the LZMA stream at the start of `input` (of the data size `size`, or None for one
/// ending with an end marker), its data at most `limit` bytes, its record cut into parts of
/// [`PART`] bytes coded on up to `threads` threads. Fails where the stream does not decode, or
/// is not what encoding its packets gives (a range coder ended otherwise).
pub fn split(
    input: &[u8],
    props: Props,
    dict: u32,
    size: Option<u64>,
    limit: usize,
    threads: usize,
) -> std::result::Result<Split, lzma::Error> {
    split_with(input, props, dict, size, limit, PART, threads)
}

/// [`split`] with parts of `part` bytes: the record is the same whatever the threads.
pub fn split_with(
    input: &[u8],
    props: Props,
    dict: u32,
    size: Option<u64>,
    limit: usize,
    part: usize,
    threads: usize,
) -> std::result::Result<Split, lzma::Error> {
    split_at(input, props, dict, size, limit, part, threads, None)
}

/// [`split_with`] with the match finder of the reach `fin` ([`finder_of`]); None for the
/// largest that the memory bound allows ([`crate::plan::finder_for`]).
#[allow(clippy::too_many_arguments)]
pub fn split_at(
    input: &[u8],
    props: Props,
    dict: u32,
    size: Option<u64>,
    limit: usize,
    part: usize,
    threads: usize,
    fin: Option<u8>,
) -> std::result::Result<Split, lzma::Error> {
    let dec = lzma::decode(input, props, size, limit)?;
    if lzma::encode(&dec.data, props, &dec.packets, dec.marker) != input[..dec.used] {
        return Err(lzma::Error::Encoded);
    }
    let fin = fin.unwrap_or_else(|| crate::plan::finder_for(dec.data.len(), dict, crate::plan::Own::Lzma(props)));
    let parts = parts_of(&dec.packets, part.max(1));
    let body = match parts.len() {
        1 => deconstruct(&dec.data, &dec.packets, dict, fin, None).map(|(b, _)| b),
        _ => deconstruct_parts(&dec.data, &dec.packets, dict, fin, &parts, threads),
    };
    let body = body.ok_or(lzma::Error::Corrupt)?;
    let mut record = Vec::with_capacity(HEADER + body.len());
    record.push(props.byte());
    record.extend(dict.to_le_bytes());
    record.push(dec.marker as u8 | if parts.len() > 1 { 2 } else { 0 });
    record.push(fin);
    record.extend(body);
    Ok(Split { data: dec.data, record, used: dec.used })
}

/// What a record tells of a stream before it is rebuilt: its properties, its dictionary size,
/// the match finder's reach ([`finder_of`]), whether it ends with a marker, its number of parts
/// (1, or `parts_count` of at least 2) and the body.
pub struct Head<'a> {
    pub props: Props,
    pub dict: u32,
    pub fin: u8,
    pub marker: bool,
    pub parts: usize,
    pub body: &'a [u8],
}

/// The [`Head`] of the record of a stream of `n` bytes of data. None where it is not one.
pub fn head_of(record: &[u8], n: usize) -> Option<Head<'_>> {
    let (head, body) = record.split_at_checked(HEADER)?;
    let props = Props::from_byte(head[0])?;
    let dict = u32::from_le_bytes(head[1..5].try_into().unwrap());
    let fin = head[6];
    finder_of(dict, fin)?;
    let parts = match head[5] {
        0 | 1 => 1,
        2 | 3 => parts_count(body, n)?,
        _ => return None,
    };
    Some(Head { props, dict, fin, marker: head[5] & 1 != 0, parts, body })
}

/// The stream of `data` from its record, its parts on up to `threads` threads. None where the
/// record is not one of `data`'s.
pub fn join(data: &[u8], record: &[u8], threads: usize) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(data.len() / 4);
    join_onto(data, record, threads, &mut out)?;
    Some(out)
}

/// [`join`] written onto the end of `out`.
pub fn join_onto(data: &[u8], record: &[u8], threads: usize, out: &mut Vec<u8>) -> Option<()> {
    let h = head_of(record, data.len())?;
    let one;
    let many;
    let parts: &[PartRec] = match h.parts {
        1 => {
            one = [PartRec { part: Part::START, end: data.len(), body: h.body }];
            &one
        }
        _ => {
            many = parts_in(h.body, data.len(), reach(h.dict))?;
            &many
        }
    };
    rebuild_parts(data, parts, h.props, h.dict, h.fin, h.marker, threads, out)
}

/// [`join`] as a part module does it: the `n` bytes of data read in order from `data` into a
/// window, the record read in order from `rec` (`rec_len` bytes of it, the stream's), the stream
/// put to `sink` in pieces; nothing of the lengths is held. The same bytes, one thread.
pub fn join_stream(data: &mut dyn Pull, n: usize, rec: &mut Reader, rec_len: u64, sink: &mut dyn Put) -> Result<()> {
    let taken = rec.taken();
    let mut ok = || -> Option<()> {
        let props = Props::from_byte(rec.byte()?)?;
        let mut dict = [0; 4];
        for b in &mut dict {
            *b = rec.byte()?;
        }
        let dict = u32::from_le_bytes(dict);
        let flags = rec.byte()?;
        let fin = rec.byte()?;
        let finder = finder_of(dict, fin)?;
        let body = rec_len.checked_sub(HEADER as u64)?;
        let parts = match flags {
            0 | 1 => 1,
            2 | 3 => {
                let k = usize::try_from(rec.num().ok()?).ok();
                k.filter(|&k| k >= 1 && k < n && k as u64 <= body / 7)? + 1
            }
            _ => return None,
        };
        let reach = reach(dict);
        let mut win = Window::fed(data, n, reach);
        let mut mf = Mf::new(n, finder);
        let mut e = StreamEncoder::to(props, sink);
        let mut pos = 0usize;
        for k in 0..parts {
            let (mut part, mut end, mut len) = (Part::START, n, body);
            if parts > 1 {
                let span = usize::try_from(rec.num().ok()?).ok().filter(|&s| s > 0 && s <= n - pos)?;
                len = rec.num().ok()?;
                end = pos + span;
                if k > 0 {
                    let state = rec.byte().filter(|&s| s < 12)?;
                    let mut reps = [0u32; 4];
                    for r in &mut reps {
                        *r = u32::try_from(rec.num().ok()?).ok().filter(|&r| (r as usize) < reach)?;
                    }
                    part = Part { at: pos, state, reps };
                }
            }
            let mut d = Body::new(rec, len);
            let on = |v: &View, ev| {
                if let Ev::Packet(pk) = ev {
                    e.push(v, pk);
                }
                Some(())
            };
            walk(&mut d, &mut mf, &mut win, reach, Chunks::Off, part, end, None, on)?;
            d.finish()?;
            pos = end;
        }
        win.finish()?;
        e.finish_to(&win.view(), flags & 1 != 0)?;
        (pos == n && rec.taken() - taken == rec_len).then_some(())
    };
    ok().ok_or_else(corrupt)
}
