//! The record of an LZMA stream's parse given its data.
//!
//! A match's distance is not coded where it is the nearest one matching for its length (what a
//! binary-tree match finder offers an optimal parser). Lengths are coded against the longest
//! match there, and packet types under contexts of what the data offers at the position. Both
//! sides see the same data, so the contexts are the same.

use aqr_common::varint;

use crate::arith::{ArithDecoder, ArithEncoder, Bit};
use crate::lzma::{self, Kind, MAX_LEN, MIN_LEN, Packet, Props, StreamEncoder};
use crate::mf::{DEPTH, Mf, lcp_up};

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
}
pub const CATS: usize = 4;

struct Tree9([Bit; 512]);

impl Default for Tree9 {
    fn default() -> Self {
        Tree9([Bit::default(); 512])
    }
}

/// Bits of the contexts held in one allocation ([`Ctx`]): the lengths of the lists of
/// probabilities, in order.
pub const CTX_LENS: [usize; 11] = [12 * 64, 12 * 64, 12 * 64, 12 * 8, 64, 64, 8 * 16, 4 * 8, 8 * 512, 8 * 512, 64];
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
        let [a, b, c, d, e, f, g, h, i, j, k] = CTX_LENS.map(&mut take);
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
        }
    }
}

/// The side of the record's coder, as [`crate::rc::Bits`] for LZMA's.
trait Side {
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

/// A place in the data where packets end and none goes past (the start of an LZMA2 chunk), in
/// order: there the state and the last distances may be reset, and bytes stored as they are
/// (not coded as packets) skipped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Break {
    pub at: usize,
    pub reset: bool,
    pub stored: usize,
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

/// Walks the packets of `data` from `part` to `end` (a packet's start, or the end of the data)
/// with matches at most `reach` back, under contexts of their own and with `mf` (holding the
/// positions within reach before `part`), coding them (`Some`) or decoding them (`None`, until
/// `end` is reached), and gives each to `emit`. None where the record does not fit the data, or
/// a packet goes past a break or `end`. Breaks are of a walk over all the data.
#[allow(clippy::too_many_arguments)]
fn walk<S: Side>(
    s: &mut S,
    mf: &mut Mf,
    data: &[u8],
    reach: usize,
    breaks: &[Break],
    part: Part,
    end: usize,
    mut given: Option<&[Packet]>,
    mut emit: impl FnMut(Packet),
) -> Option<()> {
    if !breaks.is_empty() && (part.at != 0 || end != data.len()) {
        return None;
    }
    // (the contexts, then the pairs: dropped in the reverse order)
    let mut store = vec![Bit::default(); CTX_BITS];
    let mut x = Ctx::new(&mut store);
    let (mut state, mut reps) = (part.state as usize, part.reps);
    let mut pairs = Vec::with_capacity(PAIRS);
    let mut pos = part.at;
    let (mut i, mut bi) = (0, 0);
    loop {
        while let Some(b) = breaks.get(bi).filter(|b| b.at <= pos) {
            if b.at < pos {
                return None;
            }
            if b.reset {
                (state, reps) = (0, [0; 4]);
            }
            pos = pos.checked_add(b.stored)?;
            bi += 1;
        }
        if pos >= end {
            break;
        }
        // (packets end at the next break)
        let end = breaks.get(bi).map_or(data.len(), |b| b.at.min(data.len()));
        let pk = match &mut given {
            Some(g) => *g.get(i)?,
            None => Packet::LIT,
        };
        i += 1;
        mf.advance(pos);
        let cap = (MAX_LEN as usize).min(end - pos);
        let mut rl = [0usize; 4];
        for k in 0..4 {
            if (reps[k] as usize) < pos {
                rl[k] = lcp_up(data, pos - reps[k] as usize - 1, pos, cap);
            }
        }
        let hint = mf.first(pos, cap);
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
            emit(Packet::LIT);
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
                    emit(Packet::SHORT_REP);
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
            mf.longest(pos, DEPTH, NICE, cap);
            mf.pairs(pos, cap, &mut pairs);
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
            let near = mf.nearest(pos, n, LIMIT);
            let is_near = near.is_some_and(|q| pk.dist as usize == pos - q - 1);
            let d = if s.bit(&mut x.far, !is_near, Cat::Dist) {
                // The last distance nearest to it, the sign, and the offset by its bit length.
                let off = |k: usize| pk.dist as i64 - reps[k] as i64;
                let k = (0..4).min_by_key(|&k| off(k).unsigned_abs()).unwrap();
                let k = s.tree(&mut x.far_rep, 2, k as u32, Cat::Dist) as usize;
                let neg = s.bit(&mut x.far_sign[k], off(k) < 0, Cat::Dist);
                let m = off(k).unsigned_abs() as u32;
                let b = s.tree(&mut x.far_bucket, 6, 32 - m.leading_zeros(), Cat::Dist);
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
            if d as usize >= pos.min(reach) || lcp_up(data, pos - d as usize - 1, pos, n) < n {
                return None;
            }
            reps.copy_within(0..3, 1);
            reps[0] = d;
            state = if state < 7 { 7 } else { 10 };
            Packet { kind: Kind::Match, len: n as u16, dist: d }
        };
        emit(pk);
        pos += pk.len as usize;
    }
    (pos == end && (end < data.len() || bi == breaks.len()) && given.is_none_or(|g| g.len() == i)).then_some(())
}

/// The record of the packets of `data` (from [`crate::lzma::decode`]) for a dictionary of
/// `dict` bytes, with `breaks` (none for LZMA), and its cost by [`Cat`] in bits. None where the
/// packets do not cover the data exactly.
pub fn deconstruct(data: &[u8], packets: &[Packet], dict: u32, breaks: &[Break]) -> Option<(Vec<u8>, [f64; CATS])> {
    let mut e = Enc::with(ArithEncoder::new(vec![]));
    let mut mf = Mf::new(data, reach(dict));
    walk(&mut e, &mut mf, data, reach(dict), breaks, Part::START, data.len(), Some(packets), |_| ())?;
    Some((e.c.finish(), e.cost))
}

/// The packets of `data` from its record, given to `emit` as they come. None where the record
/// does not fit the data (some packets may have been given by then).
pub fn reconstruct(data: &[u8], record: &[u8], dict: u32, breaks: &[Break], emit: impl FnMut(Packet)) -> Option<()> {
    let mut d = ArithDecoder::new(record);
    let mut mf = Mf::new(data, reach(dict));
    walk(&mut d, &mut mf, data, reach(dict), breaks, Part::START, data.len(), None, emit)?;
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
    parts: &[(Part, usize)],
    threads: usize,
) -> Option<Vec<u8>> {
    let reach = reach(dict);
    let code = |k: usize, mf: &mut Mf| {
        let (part, first) = parts[k];
        let (end, last) = parts.get(k + 1).map_or((data.len(), packets.len()), |&(p, i)| (p.at, i));
        let mut e = Enc::with(ArithEncoder::new(vec![]));
        walk(&mut e, mf, data, reach, &[], part, end, Some(&packets[first..last]), |_| ())?;
        Some(e.c.finish())
    };
    let bodies: Vec<Option<Vec<u8>>> = if threads <= 1 || Mf::at_once(data.len(), reach) < 2 {
        let mut mf = Mf::new(data, reach);
        (0..parts.len()).map(|k| code(k, &mut mf)).collect()
    } else {
        aqr_common::pool::par_map(parts.len(), threads, |k| {
            code(k, &mut Mf::starting(data, reach, parts[k].0.at.saturating_sub(reach)))
        })
    };
    let bodies: Vec<Vec<u8>> = bodies.into_iter().collect::<Option<_>>()?;
    let mut out = Vec::new();
    varint::put(&mut out, parts.len() as u64 - 1);
    for w in parts.windows(2) {
        let p = w[1].0;
        varint::put(&mut out, (p.at - w[0].0.at) as u64);
        out.push(p.state);
        p.reps.iter().for_each(|&r| varint::put(&mut out, r as u64));
    }
    bodies[..bodies.len() - 1].iter().for_each(|b| varint::put(&mut out, b.len() as u64));
    bodies.iter().for_each(|b| out.extend_from_slice(b));
    Some(out)
}

/// The number of parts of a body of a record cut into them (see [`deconstruct_parts`]) for data
/// of `n` bytes: the first varint, and one; None where it is not a count of them.
fn parts_count(body: &[u8], n: usize) -> Option<usize> {
    let k =
        usize::try_from(varint::get(body, &mut 0).ok()?).ok().filter(|&k| k >= 1 && k < n && k <= body.len() / 7)?;
    Some(k + 1)
}

/// The parts of a body of a record cut into them (see [`deconstruct_parts`]), each with its
/// body, in a vector of exactly [`parts_count`] of them; None where the body is not one of data
/// of `n` bytes.
fn parts_in(body: &[u8], n: usize) -> Option<Vec<(Part, &[u8])>> {
    let mut at = 0;
    let get = |at: &mut usize| varint::get(body, at).ok();
    let count = parts_count(body, n)?;
    get(&mut at)?;
    let mut parts = Vec::with_capacity(count);
    parts.push((Part::START, &body[..0]));
    for _ in 1..count {
        let gap = usize::try_from(get(&mut at)?).ok().filter(|&g| g > 0)?;
        let pos = parts.last()?.0.at.checked_add(gap).filter(|&p| p < n)?;
        let state = *body.get(at).filter(|&&s| s < 12)?;
        at += 1;
        let mut reps = [0u32; 4];
        for r in &mut reps {
            *r = u32::try_from(get(&mut at)?).ok()?;
        }
        parts.push((Part { at: pos, state, reps }, &body[..0]));
    }
    // (the bodies' lengths, each but the last's; the bodies themselves after them)
    let mut lens_at = at;
    for _ in 1..count {
        get(&mut at)?;
    }
    let mut rest = body.get(at..)?;
    for part in &mut parts[..count - 1] {
        let len = usize::try_from(get(&mut lens_at)?).ok()?;
        let (b, r) = rest.split_at_checked(len)?;
        rest = r;
        part.1 = b;
    }
    parts[count - 1].1 = rest;
    Some(parts)
}

/// The stream of `data` from the `parts` of its record (each with its body), on up to `threads`
/// threads where more than one match finder works at once ([`Mf::at_once`]: the parts' packets
/// decoded side by side, then encoded in order), with the end marker if `marker`; else one goes
/// through the parts with one match finder. Written onto the end of `out`.
fn rebuild_parts(
    data: &[u8],
    parts: &[(Part, &[u8])],
    props: Props,
    dict: u32,
    marker: bool,
    threads: usize,
    out: &mut Vec<u8>,
) -> Option<()> {
    let reach = reach(dict);
    let end = |k: usize| parts.get(k + 1).map_or(data.len(), |p| p.0.at);
    let mut e = StreamEncoder::new(props, std::mem::take(out));
    if threads <= 1 || parts.len() == 1 || Mf::at_once(data.len(), reach) < 2 {
        let mut mf = Mf::new(data, reach);
        for (k, &(part, b)) in parts.iter().enumerate() {
            let mut d = ArithDecoder::new(b);
            walk(&mut d, &mut mf, data, reach, &[], part, end(k), None, |pk| e.push(data, pk))?;
            if d.overrun() {
                return None;
            }
        }
    } else {
        let packets = aqr_common::pool::par_map(parts.len(), threads, |k| {
            let (part, b) = parts[k];
            let mut d = ArithDecoder::new(b);
            let mut mf = Mf::starting(data, reach, part.at.saturating_sub(reach));
            let mut out = Vec::new();
            walk(&mut d, &mut mf, data, reach, &[], part, end(k), None, |pk| out.push(pk))?;
            (!d.overrun()).then_some(out)
        });
        for p in packets {
            p?.into_iter().for_each(|pk| e.push(data, pk));
        }
    }
    *out = e.finish(data, marker);
    Some(())
}

/// The stream of `data` from its record, with the end marker if `marker`.
pub fn rebuild(data: &[u8], record: &[u8], props: Props, dict: u32, marker: bool) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(data.len() / 4);
    rebuild_parts(data, &[(Part::START, record)], props, dict, marker, 1, &mut out)?;
    Some(out)
}

/// How far back a match may be: decoders keep at least 4 KiB.
pub fn reach(dict: u32) -> usize {
    dict.max(1 << 12) as usize
}

/// The bytes of a record before its body: the properties byte, the dictionary size (le32) and
/// the flags (bit 0: the stream ends with an end marker).
pub const HEADER: usize = 6;

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
) -> Result<Split, lzma::Error> {
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
) -> Result<Split, lzma::Error> {
    let dec = lzma::decode(input, props, size, limit)?;
    if lzma::encode(&dec.data, props, &dec.packets, dec.marker) != input[..dec.used] {
        return Err(lzma::Error::Encoded);
    }
    let parts = parts_of(&dec.packets, part.max(1));
    let body = match parts.len() {
        1 => deconstruct(&dec.data, &dec.packets, dict, &[]).map(|(b, _)| b),
        _ => deconstruct_parts(&dec.data, &dec.packets, dict, &parts, threads),
    };
    let body = body.ok_or(lzma::Error::Corrupt)?;
    let mut record = Vec::with_capacity(HEADER + body.len());
    record.push(props.byte());
    record.extend(dict.to_le_bytes());
    record.push(dec.marker as u8 | if parts.len() > 1 { 2 } else { 0 });
    record.extend(body);
    Ok(Split { data: dec.data, record, used: dec.used })
}

/// What a record tells of a stream before it is rebuilt: its properties, its dictionary size,
/// whether it ends with a marker, its number of parts (1, or [`parts_count`] of at least 2) and the
/// body.
pub struct Head<'a> {
    pub props: Props,
    pub dict: u32,
    pub marker: bool,
    pub parts: usize,
    pub body: &'a [u8],
}

/// The [`Head`] of the record of a stream of `n` bytes of data. None where it is not one.
pub fn head_of(record: &[u8], n: usize) -> Option<Head<'_>> {
    let (head, body) = record.split_at_checked(HEADER)?;
    let props = Props::from_byte(head[0])?;
    let dict = u32::from_le_bytes(head[1..5].try_into().unwrap());
    let parts = match head[5] {
        0 | 1 => 1,
        2 | 3 => parts_count(body, n)?,
        _ => return None,
    };
    Some(Head { props, dict, marker: head[5] & 1 != 0, parts, body })
}

/// The stream of `data` from its record, its parts on up to `threads` threads. None where the
/// record is not one of `data`'s.
pub fn join(data: &[u8], record: &[u8], threads: usize) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(data.len() / 4);
    join_onto(data, record, threads, &mut out)?;
    Some(out)
}

/// [`join`] written onto the end of `out`: where it has the room, it takes no more memory than
/// the stream's rebuilding does ([`crate::plan`]). None also leaves `out` as it was not.
pub fn join_onto(data: &[u8], record: &[u8], threads: usize, out: &mut Vec<u8>) -> Option<()> {
    let h = head_of(record, data.len())?;
    let one;
    let many;
    let parts: &[(Part, &[u8])] = match h.parts {
        1 => {
            one = [(Part::START, h.body)];
            &one
        }
        _ => {
            many = parts_in(h.body, data.len())?;
            &many
        }
    };
    rebuild_parts(data, parts, h.props, h.dict, h.marker, threads, out)
}
