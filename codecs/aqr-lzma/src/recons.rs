//! The record of an LZMA stream's parse given its data.
//!
//! A match's distance is not coded where it is the nearest one matching for its length (what a
//! binary-tree match finder offers an optimal parser). Lengths are coded against the longest
//! match there, and packet types under contexts of what the data offers at the position. Both
//! sides see the same data, so the contexts are the same.

use crate::arith::{ArithDecoder, ArithEncoder, Bit};
use crate::lzma::{self, Kind, MAX_LEN, MIN_LEN, Packet, Props, StreamEncoder};
use crate::mf::{Mf, lcp};

/// Candidates looked at for the longest match at a match, on each of two chains.
const DEPTH: usize = 16;
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

#[derive(Default)]
struct Ctx {
    is_match: Vec<Bit>,
    is_rep: Vec<Bit>,
    g0: Vec<Bit>,
    rep0_long: Vec<Bit>,
    g1: Vec<Bit>,
    g2: Vec<Bit>,
    /// A match: which pair (from the longest), whether at its length, else how much shorter.
    pair: Vec<Bit>,
    pair_more: Tree9,
    pair_full: Vec<Bit>,
    pair_short: Vec<Tree9>,
    match_len: Tree9,
    far: Bit,
    /// A distance not found: its offset from the nearest of the last four.
    far_rep: [Bit; 4],
    far_sign: [Bit; 4],
    far_bucket: Vec<Bit>,
    rep_full: [[Bit; 8]; 4],
    rep_len: Vec<Tree9>,
}

impl Ctx {
    fn new() -> Self {
        let v = |n| vec![Bit::default(); n];
        Ctx {
            is_match: v(12 * 64),
            is_rep: v(12 * 64),
            g0: v(12 * 64),
            rep0_long: v(12 * 8),
            g1: v(64),
            g2: v(64),
            pair: v(8 * 16),
            pair_full: v(4 * 8),
            pair_short: (0..8).map(|_| Tree9::default()).collect(),
            rep_len: (0..8).map(|_| Tree9::default()).collect(),
            far_bucket: v(64),
            ..Default::default()
        }
    }
}

/// The side of the record's coder, as [`crate::rc::Bits`] for LZMA's.
trait Side {
    fn bit(&mut self, b: &mut Bit, v: bool, cat: Cat) -> bool;
    /// `n` bits of probability 1/2.
    fn raw(&mut self, n: u32, v: u32, cat: Cat) -> u32 {
        let mut x = 0;
        for i in (0..n).rev() {
            x = x << 1 | self.bit(&mut Bit::default(), v >> i & 1 != 0, cat) as u32;
        }
        x
    }
    /// `n` bits under the contexts of the bits before them.
    fn tree(&mut self, t: &mut [Bit], n: u32, v: u32, cat: Cat) -> u32 {
        let mut m = 1;
        for i in (0..n).rev() {
            m = m << 1 | self.bit(&mut t[m], v >> i & 1 != 0, cat) as usize;
        }
        m as u32 - (1 << n)
    }

    fn tree9(&mut self, t: &mut Tree9, v: u32, cat: Cat) -> u32 {
        let mut m = 1;
        for i in (0..9).rev() {
            m = m << 1 | self.bit(&mut t.0[m], v >> i & 1 != 0, cat) as usize;
        }
        m as u32 - 512
    }
}

struct Enc {
    c: ArithEncoder,
    cost: [f64; CATS],
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

/// Walks the packets of `data` with matches at most `reach` back, coding them (`Some`) or
/// decoding them (`None`, until the data is covered), and gives each to `emit`. None where
/// the record does not fit the data.
fn walk<S: Side>(
    s: &mut S,
    data: &[u8],
    reach: usize,
    mut given: Option<&[Packet]>,
    mut emit: impl FnMut(Packet),
) -> Option<()> {
    let mut mf = Mf::new(data, reach);
    let mut x = Ctx::new();
    let (mut state, mut reps) = (0usize, [0u32; 4]);
    let mut pairs = vec![];
    let mut pos = 0;
    let mut i = 0;
    while pos < data.len() {
        let pk = match &mut given {
            Some(g) => *g.get(i)?,
            None => Packet::LIT,
        };
        i += 1;
        mf.advance(pos);
        let cap = (MAX_LEN as usize).min(data.len() - pos);
        let mut rl = [0usize; 4];
        for k in 0..4 {
            if (reps[k] as usize) < pos {
                rl[k] = lcp(data, pos - reps[k] as usize - 1, pos, cap);
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
                s.tree9(&mut x.rep_len[bucket(l)], (pk.len as u32).wrapping_sub(MIN_LEN), Cat::RepLen) as usize
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
                jj += s.tree9(&mut x.pair_more, j.wrapping_sub(15) as u32, Cat::Len) as usize;
            }
            let n = if jj >= m {
                // Longer than any found.
                let top = pairs.last().map_or(MIN_LEN as usize - 1, |&(l, _)| l as usize);
                let n = s.tree9(&mut x.match_len, (pk.len as u32).wrapping_sub(MIN_LEN), Cat::Len) as usize
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
                    let v =
                        s.tree9(&mut x.pair_short[bucket(top)], (top as u32).wrapping_sub(pk.len as u32 + 1), Cat::Len);
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
            if d as usize >= pos.min(reach) || lcp(data, pos - d as usize - 1, pos, n) < n {
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
    given.is_none_or(|g| g.len() == i).then_some(())
}

/// The record of the packets of `data` (from [`crate::lzma::decode`]) for a dictionary of
/// `dict` bytes, and its cost by [`Cat`] in bits. None where the packets do not cover the data
/// exactly.
pub fn deconstruct(data: &[u8], packets: &[Packet], dict: u32) -> Option<(Vec<u8>, [f64; CATS])> {
    let mut e = Enc { c: ArithEncoder::new(vec![]), cost: [0.0; CATS] };
    walk(&mut e, data, reach(dict), Some(packets), |_| ())?;
    Some((e.c.finish(), e.cost))
}

/// The packets of `data` from its record, given to `emit` as they come. None where the record
/// does not fit the data (some packets may have been given by then).
pub fn reconstruct(data: &[u8], record: &[u8], dict: u32, emit: impl FnMut(Packet)) -> Option<()> {
    let mut d = Dec { c: ArithDecoder::new(record) };
    walk(&mut d, data, reach(dict), None, emit)?;
    (!d.c.overrun()).then_some(())
}

/// The stream of `data` from its record, with the end marker if `marker`.
pub fn rebuild(data: &[u8], record: &[u8], props: Props, dict: u32, marker: bool) -> Option<Vec<u8>> {
    let mut e = StreamEncoder::new(props, data.len() / 4);
    reconstruct(data, record, dict, |pk| e.push(data, pk))?;
    Some(e.finish(data, marker))
}

/// How far back a match may be: decoders keep at least 4 KiB.
fn reach(dict: u32) -> usize {
    dict.max(1 << 12) as usize
}

/// The bytes of a record before its body: the properties byte, the dictionary size (le32) and
/// the flags (bit 0: the stream ends with an end marker).
pub const HEADER: usize = 6;

/// A stream split into its data and its record.
pub struct Split {
    pub data: Vec<u8>,
    pub record: Vec<u8>,
    /// The bytes of the input the stream took (what follows is not the stream's).
    pub used: usize,
}

/// Splits the LZMA stream at the start of `input` (of the data size `size`, or None for one
/// ending with an end marker), its data at most `limit` bytes. Fails where the stream does not
/// decode, or is not what encoding its packets gives (a range coder ended otherwise).
pub fn split(input: &[u8], props: Props, dict: u32, size: Option<u64>, limit: usize) -> Result<Split, lzma::Error> {
    let dec = lzma::decode(input, props, size, limit)?;
    if lzma::encode(&dec.data, props, &dec.packets, dec.marker) != input[..dec.used] {
        return Err(lzma::Error::Encoded);
    }
    let (body, _) = deconstruct(&dec.data, &dec.packets, dict).ok_or(lzma::Error::Corrupt)?;
    let mut record = Vec::with_capacity(HEADER + body.len());
    record.push(props.byte());
    record.extend(dict.to_le_bytes());
    record.push(dec.marker as u8);
    record.extend(body);
    Ok(Split { data: dec.data, record, used: dec.used })
}

/// The stream of `data` from its record. None where the record is not one of `data`'s.
pub fn join(data: &[u8], record: &[u8]) -> Option<Vec<u8>> {
    let (head, body) = record.split_at_checked(HEADER)?;
    let props = Props::from_byte(head[0])?;
    let dict = u32::from_le_bytes(head[1..5].try_into().unwrap());
    if head[5] > 1 {
        return None;
    }
    rebuild(data, body, props, dict, head[5] != 0)
}
