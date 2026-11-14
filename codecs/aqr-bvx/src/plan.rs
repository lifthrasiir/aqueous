//! A stream as its data, its parse (the matches) and the little else its blocks hold
//! (`FORMAT.md` §5): [`split`] and [`join`].

use aqr_common::varint;

use crate::bvx::{self, Block, D_BASE, Freqs, L_BASE, L_MAX, Lzfse, LzfseRef, M_BASE, M_MAX, Stream, symbol};
use crate::fse::normalize;
use crate::lzvn;
pub use crate::lzvn::Match;
use crate::{Error, Result, corrupt};

/// What an LZFSE block holds beyond its triples and literals, which the compressor's usual
/// choices determine but for these.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Side {
    /// Whether its frequencies are its own (the next of the plan's `freqs`), not those normalized
    /// from the counts.
    pub own: bool,
    pub lit_start: [u32; 4],
    pub lmd_start: [u32; 3],
    pub lit_low: u32,
    pub lmd_low: u32,
    /// The literals padding the block's to a multiple of 4: the first `npad`.
    pub pad: [u8; 3],
    pub npad: u8,
}

/// The triples' stream's zero bits below its symbols, as the compressor leaves them.
pub const LMD_LOW: u32 = 64;

impl Side {
    /// What the compressor writes (but for the pad).
    pub const USUAL: Side =
        Side { own: false, lit_start: [0; 4], lmd_start: [0; 3], lit_low: 0, lmd_low: LMD_LOW, pad: [0; 3], npad: 0 };

    /// Whether it is what the compressor writes, but for its pad.
    pub fn usual(&self) -> bool {
        !self.own
            && self.lit_start == [0; 4]
            && self.lmd_start == [0; 3]
            && self.lit_low == 0
            && self.lmd_low == LMD_LOW
    }

    /// The pad.
    pub fn pad(&self) -> &[u8] {
        &self.pad[..self.npad as usize]
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Part {
    Stored(u32),
    /// An LZVN block of so many bytes of data.
    Lzvn(u32),
    /// An LZFSE block of so many triples.
    Lzfse(u32, Side),
}

/// The parse of a segment (the data of an LZVN block, or of LZFSE blocks one after another):
/// its matches, those of the plan's up to `end` past the segment's before, then `tail`
/// literals.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Seg {
    pub end: u32,
    pub tail: u32,
}

/// A stream's plan, its vectors flat (each in the order of the stream).
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Plan {
    pub parts: Vec<Part>,
    /// The parses of the segments in order.
    pub segs: Vec<Seg>,
    pub matches: Vec<Match>,
    /// The frequencies of the LZFSE blocks of their own ([`Side::own`]).
    pub freqs: Vec<Freqs>,
    /// The distance field of each triple of no match of the LZFSE blocks.
    pub zd: Vec<u32>,
}

impl Plan {
    /// A plan of nothing, of room for exactly `c`'s.
    pub fn with_room(c: &Counts) -> Plan {
        Plan {
            parts: Vec::with_capacity(c.parts),
            segs: Vec::with_capacity(c.segs),
            matches: Vec::with_capacity(c.matches),
            freqs: Vec::with_capacity(c.freqs),
            zd: Vec::with_capacity(c.zd),
        }
    }

    /// The parse of segment `i`: its matches, and its literals after them.
    pub fn seg(&self, i: usize) -> Result<(&[Match], u32)> {
        let start = if i == 0 { 0 } else { self.segs.get(i - 1).ok_or_else(corrupt)?.end as usize };
        let s = self.segs.get(i).ok_or_else(corrupt)?;
        Ok((self.matches.get(start..s.end as usize).ok_or_else(corrupt)?, s.tail))
    }

    /// Appends a segment's parse.
    pub fn push_seg(&mut self, matches: &[Match], tail: u32) {
        self.matches.extend_from_slice(matches);
        self.segs.push(Seg { end: self.matches.len() as u32, tail });
    }

    /// Its counts.
    pub fn counts(&self) -> Result<Counts> {
        let (mut lmd, mut lits) = (0, 0);
        shapes(self, &mut |i, l, _| {
            let Part::Lzfse(n, side) = self.parts[i] else { unreachable!() };
            lmd = lmd.max(n as usize);
            lits = lits.max(l + side.npad as usize);
            Ok(())
        })?;
        Ok(Counts {
            parts: self.parts.len(),
            segs: self.segs.len(),
            matches: self.matches.len(),
            freqs: self.freqs.len(),
            zd: self.zd.len(),
            lmd,
            lits,
        })
    }
}

/// The lengths of a plan's vectors, and the most triples and literals (with the pad) of its
/// LZFSE blocks: what the module's params tell (`FORMAT.md` §7).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Counts {
    pub parts: usize,
    pub segs: usize,
    pub matches: usize,
    pub freqs: usize,
    pub zd: usize,
    pub lmd: usize,
    pub lits: usize,
}

impl Counts {
    fn all(&self) -> [usize; 7] {
        [self.parts, self.segs, self.matches, self.freqs, self.zd, self.lmd, self.lits]
    }

    /// As params: each a varint.
    pub fn params(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for v in self.all() {
            varint::put(&mut out, v as u64);
        }
        out
    }

    /// Of params as [`Counts::params`] writes them.
    pub fn read(params: &[u8]) -> Result<Counts> {
        let mut at = 0;
        let mut v = [0; 7];
        for x in &mut v {
            *x = usize::try_from(varint::get(params, &mut at).map_err(|_| corrupt())?).map_err(|_| corrupt())?;
        }
        if at != params.len() {
            return Err(corrupt());
        }
        let [parts, segs, matches, freqs, zd, lmd, lits] = v;
        Ok(Counts { parts, segs, matches, freqs, zd, lmd, lits })
    }
}

/// The segments of `parts`: for each, the parts it spans and whether it is LZVN's.
pub fn segments(parts: &[Part]) -> impl Iterator<Item = (std::ops::Range<usize>, bool)> + '_ {
    let mut i = 0;
    std::iter::from_fn(move || {
        while i < parts.len() {
            match parts[i] {
                Part::Stored(_) => i += 1,
                Part::Lzvn(_) => {
                    i += 1;
                    return Some((i - 1..i, true));
                }
                Part::Lzfse(..) => {
                    let j = i + parts[i..].iter().take_while(|p| matches!(p, Part::Lzfse(..))).count();
                    let r = i..j;
                    i = j;
                    return Some((r, false));
                }
            }
        }
        None
    })
}

/// Bytes of data of a segment's parse.
pub fn seg_len(matches: &[Match], tail: u32) -> u64 {
    matches.iter().map(|m| m.lits as u64 + m.len as u64).sum::<u64>() + tail as u64
}

/// The plan of the stream `st`, checked to be written again bit for bit by [`join`] into `s`.
pub fn split(st: &Stream, s: &[u8]) -> Result<Plan> {
    let mut plan = read(st)?;
    let made = blocks(&st.data, &plan)?;
    for ((p, b), made) in plan.parts.iter_mut().zip(&st.blocks).zip(&made) {
        if let (Part::Lzfse(_, side), Block::Lzfse(b), Block::Lzfse(made)) = (p, b, made)
            && made.freqs != b.freqs
        {
            side.own = true;
            plan.freqs.push(b.freqs.clone());
        }
    }
    let mut out = Vec::new();
    join(&st.data, &plan, &plan.counts()?, &mut out)?;
    if out != s[..st.used] {
        return Err(Error("stream not given back"));
    }
    Ok(plan)
}

/// The plan that the blocks of `st` read as, the frequencies taken as normalized.
pub fn read(st: &Stream) -> Result<Plan> {
    let mut plan = Plan::default();
    // an LZFSE segment: where its matches start, run, in a match
    let mut pending: Option<(usize, u32, bool)> = None;
    let close = |pending: &mut Option<(usize, u32, bool)>, plan: &mut Plan| {
        if let Some((_, run, _)) = pending.take() {
            plan.segs.push(Seg { end: plan.matches.len() as u32, tail: run });
        }
    };
    for b in &st.blocks {
        match b {
            Block::Stored(n) => {
                close(&mut pending, &mut plan);
                plan.parts.push(Part::Stored(*n));
            }
            Block::Lzvn(n, payload) => {
                close(&mut pending, &mut plan);
                let (matches, tail) = lzvn::matches(&lzvn::ops(payload)?);
                plan.parts.push(Part::Lzvn(*n));
                plan.push_seg(&matches, tail);
            }
            Block::Lzfse(b) => {
                let (start, run, joined) = pending.get_or_insert((plan.matches.len(), 0, false));
                let mut prev = 0;
                for &[l, m, d] in &b.lmd {
                    if d != 0 {
                        prev = d;
                    }
                    *run += l;
                    if m == 0 {
                        plan.zd.push(d);
                        *joined = false;
                        continue;
                    }
                    let in_seg = plan.matches.len() > *start;
                    match plan.matches.last_mut() {
                        Some(x) if in_seg && *joined && *run == 0 && x.dist == prev => x.len += m,
                        _ => plan.matches.push(Match { lits: *run, len: m, dist: prev }),
                    }
                    *run = 0;
                    // (a match continues only past a triple of the longest match)
                    *joined = m == M_MAX;
                }
                let nl: u32 = b.lmd.iter().map(|t| t[0]).sum();
                let pad = &b.lits[nl as usize..];
                if pad.len() > 3 {
                    return Err(corrupt());
                }
                let mut side = Side {
                    own: false,
                    lit_start: b.lit_start,
                    lmd_start: b.lmd_start,
                    lit_low: b.lit_low,
                    lmd_low: b.lmd_low,
                    pad: [0; 3],
                    npad: pad.len() as u8,
                };
                side.pad[..pad.len()].copy_from_slice(pad);
                plan.parts.push(Part::Lzfse(b.lmd.len() as u32, side));
            }
        }
    }
    close(&mut pending, &mut plan);
    Ok(plan)
}

/// A triple of an LZFSE block: literals, match length and distance (absolute; `None` for one
/// of no match).
pub type Triple = (u32, u32, Option<u32>);

/// The triples of a segment's parse, as the compressor cuts runs of literals and matches.
pub fn triples(matches: &[Match], tail: u32) -> impl Iterator<Item = Triple> + '_ {
    let pieces = |(r, n, d): Triple| {
        // runs of the longest literals first, the rest with the match (or alone)
        let k = r.saturating_sub(1) / L_MAX;
        let rem = r - k * L_MAX;
        let lits = std::iter::repeat_n((L_MAX, 0, None), k as usize);
        let matches =
            (0..n.div_ceil(M_MAX)).map(move |i| (if i == 0 { rem } else { 0 }, (n - i * M_MAX).min(M_MAX), d));
        lits.chain(matches).chain((n == 0 && rem > 0).then_some((rem, 0, None)))
    };
    matches.iter().map(|m| (m.lits, m.len, Some(m.dist))).chain([(tail, 0, None)]).flat_map(pieces)
}

/// For each LZFSE block of `plan` in order, `f` of its part's index, its literals (but its pad)
/// and its triples of no match.
pub fn shapes(plan: &Plan, f: &mut dyn FnMut(usize, usize, usize) -> Result<()>) -> Result<()> {
    for (si, (range, lzvn)) in segments(&plan.parts).enumerate() {
        if lzvn {
            continue;
        }
        let (ms, tail) = plan.seg(si)?;
        let mut ts = triples(ms, tail);
        for i in range {
            let Part::Lzfse(n, _) = plan.parts[i] else { unreachable!() };
            let (mut lits, mut none) = (0, 0);
            for _ in 0..n {
                let t = ts.next().ok_or_else(corrupt)?;
                lits += t.0 as usize;
                none += t.2.is_none() as usize;
            }
            f(i, lits, none)?;
        }
    }
    Ok(())
}

/// A block of a plan as [`each_block`] gives it.
pub enum View<'a> {
    Stored(u32),
    /// An LZVN block: its bytes of data, and its segment's parse.
    Lzvn(u32, &'a [Match], u32),
    Lzfse(LzfseRef<'a>),
}

/// The blocks of `plan` over `data` (the frequencies of LZFSE blocks their own, or normalized).
pub fn blocks(data: &[u8], plan: &Plan) -> Result<Vec<Block>> {
    let mut out = Vec::with_capacity(plan.parts.len());
    each_block(data, plan, &plan.counts()?, &mut |b, at| {
        out.push(match b {
            View::Stored(n) => Block::Stored(n),
            View::Lzvn(n, ms, tail) => {
                let mut payload = Vec::new();
                lzvn::write(&data[at..at + n as usize], ms, tail, &mut payload)?;
                Block::Lzvn(n, payload)
            }
            View::Lzfse(b) => Block::Lzfse(Box::new(Lzfse {
                lits: b.lits.to_vec(),
                lmd: b.lmd.to_vec(),
                freqs: b.freqs.clone(),
                lit_start: b.lit_start,
                lmd_start: b.lmd_start,
                lit_low: b.lit_low,
                lmd_low: b.lmd_low,
            })),
        });
        Ok(())
    })?;
    Ok(out)
}

/// Bytes that [`each_block`] takes of the heap for the counts `c`: an LZFSE block's triples and
/// literals; None where they overflow a stack's blocks.
pub fn each_block_heap(c: &Counts) -> Option<[u64; 2]> {
    let heap = [(c.lmd as u64).checked_mul(12)?, c.lits as u64];
    heap.iter().all(|&n| n < 1 << 62).then_some(heap)
}

/// The blocks of `plan` over `data` given to `f` in turn, each with where its data starts. The
/// LZFSE blocks' triples and literals must be at most `c`'s, and the most of them exactly.
pub fn each_block(
    data: &[u8],
    plan: &Plan,
    c: &Counts,
    f: &mut dyn FnMut(View<'_>, usize) -> Result<()>,
) -> Result<()> {
    // (room for the counts, but no more than the data holds: a triple takes a byte of it at least,
    // and the literals are its bytes with a block's pad of 3 at most)
    let mut lmd: Vec<[u32; 3]> = Vec::with_capacity(c.lmd.min(data.len()));
    let mut lits: Vec<u8> = Vec::with_capacity(c.lits.min(data.len() + 3));
    let (mut most_lmd, mut most_lits) = (0, 0);
    let (mut zd, mut freqs) = (plan.zd.iter(), plan.freqs.iter());
    let mut done = 0;
    let mut at = 0usize;
    let end = plan.parts.len();
    for (si, (range, lzvn)) in segments(&plan.parts).chain([(end..end, false)]).enumerate() {
        // the stored blocks before the segment
        while done < range.start {
            let Part::Stored(n) = plan.parts[done] else { unreachable!() };
            data.get(at..at + n as usize).ok_or_else(corrupt)?;
            f(View::Stored(n), at)?;
            done += 1;
            at += n as usize;
        }
        if range.is_empty() {
            if si != plan.segs.len() {
                return Err(corrupt());
            }
            break;
        }
        let (ms, tail) = plan.seg(si)?;
        if lzvn {
            let Part::Lzvn(n) = plan.parts[range.start] else { unreachable!() };
            data.get(at..at + n as usize).ok_or_else(corrupt)?;
            if seg_len(ms, tail) != n as u64 {
                return Err(corrupt());
            }
            f(View::Lzvn(n, ms, tail), at)?;
            done += 1;
            at += n as usize;
            continue;
        }
        let mut ts = triples(ms, tail);
        for p in &plan.parts[range.clone()] {
            let Part::Lzfse(n, side) = p else { unreachable!() };
            let n = *n as usize;
            if n > c.lmd {
                return Err(corrupt());
            }
            let start = at;
            lmd.clear();
            lits.clear();
            let mut prev = 0;
            for _ in 0..n {
                let (l, m, d) = ts.next().ok_or_else(corrupt)?;
                let d = match d {
                    Some(d) if d == prev => 0,
                    Some(d) => d,
                    None => *zd.next().ok_or_else(corrupt)?,
                };
                if d != 0 {
                    prev = d;
                }
                let l = data.get(at..at + l as usize).ok_or_else(corrupt)?;
                if l.len() > c.lits - lits.len() {
                    return Err(corrupt());
                }
                lits.extend_from_slice(l);
                at += l.len() + m as usize;
                lmd.push([l.len() as u32, m, d]);
            }
            let pad = side.pad();
            if pad.len() > 3 || !(lits.len() + pad.len()).is_multiple_of(4) || pad.len() > c.lits - lits.len() {
                return Err(corrupt());
            }
            lits.extend_from_slice(pad);
            (most_lmd, most_lits) = (most_lmd.max(n), most_lits.max(lits.len()));
            let counted;
            let freqs = if side.own {
                freqs.next().ok_or_else(corrupt)?
            } else {
                counted = self::counted(&lits, &lmd);
                &counted
            };
            let b = LzfseRef {
                lits: &lits,
                lmd: &lmd,
                freqs,
                lit_start: side.lit_start,
                lmd_start: side.lmd_start,
                lit_low: side.lit_low,
                lmd_low: side.lmd_low,
            };
            f(View::Lzfse(b), start)?;
            done += 1;
        }
        if ts.next().is_some() {
            return Err(corrupt());
        }
    }
    if at != data.len() || zd.next().is_some() || freqs.next().is_some() || (most_lmd, most_lits) != (c.lmd, c.lits) {
        return Err(corrupt());
    }
    Ok(())
}

/// The frequencies normalized from the counts of the symbols.
fn counted(lits: &[u8], lmd: &[[u32; 3]]) -> Freqs {
    let (mut cl, mut cm, mut cd, mut cc) = ([0u32; 20], [0u32; 20], [0u32; 64], [0u32; 256]);
    for t in lmd {
        cl[symbol(&L_BASE, t[0])] += 1;
        cm[symbol(&M_BASE, t[1])] += 1;
        cd[symbol(&D_BASE, t[2])] += 1;
    }
    for &c in lits {
        cc[c as usize] += 1;
    }
    let mut f = Freqs { l: [0; 20], m: [0; 20], d: [0; 64], lit: [0; 256] };
    // (a table of no symbols stays empty)
    if !lmd.is_empty() {
        normalize(&cl, bvx::L_STATES, &mut f.l);
        normalize(&cm, bvx::M_STATES, &mut f.m);
        normalize(&cd, bvx::D_STATES, &mut f.d);
    }
    if !lits.is_empty() {
        normalize(&cc, bvx::LIT_STATES, &mut f.lit);
    }
    f
}

/// Writes the stream of `plan` over `data` onto `out`, of the counts `c` (as [`each_block`]).
pub fn join(data: &[u8], plan: &Plan, c: &Counts, out: &mut Vec<u8>) -> Result<()> {
    each_block(data, plan, c, &mut |b, at| match b {
        View::Stored(n) => {
            bvx::write_stored(&data[at..at + n as usize], out);
            Ok(())
        }
        View::Lzvn(n, ms, tail) => {
            bvx::write_lzvn(n, out, |out| lzvn::write(&data[at..at + n as usize], ms, tail, out))
        }
        View::Lzfse(b) => bvx::write_v2(&b, out),
    })?;
    out.extend_from_slice(&bvx::END);
    Ok(())
}
