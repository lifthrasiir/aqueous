//! A stream as its data, its parse (the matches) and the little else its blocks hold
//! (`FORMAT.md` §5): [`split`] and [`join`].

use crate::bvx::{self, Block, D_BASE, Freqs, L_BASE, L_MAX, Lzfse, M_BASE, M_MAX, Stream, symbol};
use crate::fse::normalize;
use crate::lzvn;
pub use crate::lzvn::Match;
use crate::{Error, Result, corrupt};

/// What an LZFSE block holds beyond its triples and literals, which the compressor's usual
/// choices determine but for these.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Side {
    /// The frequencies, where they are not those normalized from the counts.
    pub freqs: Option<Box<Freqs>>,
    pub lit_start: [u32; 4],
    pub lmd_start: [u32; 3],
    pub lit_low: u32,
    pub lmd_low: u32,
    /// The literals padding the block's to a multiple of 4.
    pub pad: Vec<u8>,
    /// The distance field of each triple of no match, in order.
    pub zd: Vec<u32>,
}

/// The triples' stream's zero bits below its symbols, as the compressor leaves them.
pub const LMD_LOW: u32 = 64;

impl Side {
    /// Whether it is what the compressor writes, but for `pad` and `zd`.
    pub fn usual(&self) -> bool {
        self.freqs.is_none()
            && self.lit_start == [0; 4]
            && self.lmd_start == [0; 3]
            && self.lit_low == 0
            && self.lmd_low == LMD_LOW
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Part {
    Stored(u32),
    /// An LZVN block of so many bytes of data.
    Lzvn(u32),
    /// An LZFSE block of so many triples.
    Lzfse(u32, Side),
}

/// The parse of a segment: the data of an LZVN block, or of LZFSE blocks one after another.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Seg {
    pub matches: Vec<Match>,
    pub tail: u32,
}

#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Plan {
    pub parts: Vec<Part>,
    /// The parses of the segments in order.
    pub segs: Vec<Seg>,
}

/// The segments of `parts`: for each, the parts it spans and whether it is LZVN's.
pub fn segments(parts: &[Part]) -> Vec<(std::ops::Range<usize>, bool)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < parts.len() {
        match parts[i] {
            Part::Stored(_) => i += 1,
            Part::Lzvn(_) => {
                out.push((i..i + 1, true));
                i += 1;
            }
            Part::Lzfse(..) => {
                let j = i + parts[i..].iter().take_while(|p| matches!(p, Part::Lzfse(..))).count();
                out.push((i..j, false));
                i = j;
            }
        }
    }
    out
}

/// Bytes of data of a segment's parse.
pub fn seg_len(s: &Seg) -> u64 {
    s.matches.iter().map(|m| m.lits as u64 + m.len as u64).sum::<u64>() + s.tail as u64
}

/// The plan of the stream `st`, checked to be written again bit for bit by [`join`] into `s`.
pub fn split(st: &Stream, s: &[u8]) -> Result<Plan> {
    let mut plan = read(st)?;
    let made = blocks(&st.data, &plan)?;
    for ((p, b), made) in plan.parts.iter_mut().zip(&st.blocks).zip(&made) {
        if let (Part::Lzfse(_, side), Block::Lzfse(b), Block::Lzfse(made)) = (p, b, made)
            && made.freqs != b.freqs
        {
            side.freqs = Some(Box::new(b.freqs.clone()));
        }
    }
    let mut out = Vec::new();
    join(&st.data, &plan, &mut out)?;
    if out != s[..st.used] {
        return Err(Error("stream not given back"));
    }
    Ok(plan)
}

/// The plan that the blocks of `st` read as, the frequencies taken as normalized.
pub fn read(st: &Stream) -> Result<Plan> {
    let mut parts = Vec::new();
    let mut segs = Vec::new();
    let mut pending: Option<(Seg, u32, bool)> = None; // an LZFSE segment: parse, run, in a match
    let close = |pending: &mut Option<(Seg, u32, bool)>, segs: &mut Vec<Seg>| {
        if let Some((mut seg, run, _)) = pending.take() {
            seg.tail = run;
            segs.push(seg);
        }
    };
    let mut at = 0usize;
    for b in &st.blocks {
        match b {
            Block::Stored(n) => {
                close(&mut pending, &mut segs);
                parts.push(Part::Stored(*n));
                at += *n as usize;
            }
            Block::Lzvn(n, payload) => {
                close(&mut pending, &mut segs);
                let mut out = st.data[..at].to_vec();
                let p = lzvn::parse_onto(payload, &mut out, *n as usize, true)?;
                let (matches, tail) = lzvn::matches(&p.ops);
                parts.push(Part::Lzvn(*n));
                segs.push(Seg { matches, tail });
                at += *n as usize;
            }
            Block::Lzfse(b) => {
                let (seg, run, joined) = pending.get_or_insert_with(Default::default);
                let mut zd = Vec::new();
                let mut prev = 0;
                for &[l, m, d] in &b.lmd {
                    if d != 0 {
                        prev = d;
                    }
                    *run += l;
                    if m == 0 {
                        zd.push(d);
                        *joined = false;
                        continue;
                    }
                    match seg.matches.last_mut() {
                        Some(x) if *joined && *run == 0 && x.dist == prev => x.len += m,
                        _ => seg.matches.push(Match { lits: *run, len: m, dist: prev }),
                    }
                    *run = 0;
                    // (a match continues only past a triple of the longest match)
                    *joined = m == M_MAX;
                }
                let nl: u32 = b.lmd.iter().map(|t| t[0]).sum();
                let side = Side {
                    freqs: None,
                    lit_start: b.lit_start,
                    lmd_start: b.lmd_start,
                    lit_low: b.lit_low,
                    lmd_low: b.lmd_low,
                    pad: b.lits[nl as usize..].to_vec(),
                    zd,
                };
                parts.push(Part::Lzfse(b.lmd.len() as u32, side));
                at += b.lmd.iter().map(|t| (t[0] + t[1]) as usize).sum::<usize>();
            }
        }
    }
    close(&mut pending, &mut segs);
    Ok(Plan { parts, segs })
}

/// A triple of an LZFSE block: literals, match length and distance (absolute; `None` for one
/// of no match).
pub type Triple = (u32, u32, Option<u32>);

/// The triples of a segment's parse, as the compressor cuts runs of literals and matches.
pub fn triples(seg: &Seg) -> impl Iterator<Item = Triple> + '_ {
    let pieces = |(r, n, d): Triple| {
        // runs of the longest literals first, the rest with the match (or alone)
        let k = r.saturating_sub(1) / L_MAX;
        let rem = r - k * L_MAX;
        let lits = std::iter::repeat_n((L_MAX, 0, None), k as usize);
        let matches =
            (0..n.div_ceil(M_MAX)).map(move |i| (if i == 0 { rem } else { 0 }, (n - i * M_MAX).min(M_MAX), d));
        lits.chain(matches).chain((n == 0 && rem > 0).then_some((rem, 0, None)))
    };
    seg.matches.iter().map(|m| (m.lits, m.len, Some(m.dist))).chain([(seg.tail, 0, None)]).flat_map(pieces)
}

/// The blocks of `plan` over `data` (the frequencies of LZFSE blocks those of their `Side`, or
/// normalized).
pub fn blocks(data: &[u8], plan: &Plan) -> Result<Vec<Block>> {
    let mut out = Vec::with_capacity(plan.parts.len());
    each_block(data, plan, &mut |b, _| {
        out.push(b);
        Ok(())
    })?;
    Ok(out)
}

/// The blocks of `plan` over `data` given to `f` in turn, each with where its data starts.
pub fn each_block(data: &[u8], plan: &Plan, f: &mut dyn FnMut(Block, usize) -> Result<()>) -> Result<()> {
    let mut done = 0;
    let mut at = 0usize;
    let mut segs = plan.segs.iter();
    for (range, lzvn) in segments(&plan.parts).into_iter().chain([(plan.parts.len()..plan.parts.len(), false)]) {
        // the stored blocks before the segment
        while done < range.start {
            let Part::Stored(n) = plan.parts[done] else { unreachable!() };
            data.get(at..at + n as usize).ok_or_else(corrupt)?;
            f(Block::Stored(n), at)?;
            done += 1;
            at += n as usize;
        }
        if range.is_empty() {
            break;
        }
        let seg = segs.next().ok_or_else(corrupt)?;
        if lzvn {
            let Part::Lzvn(n) = plan.parts[range.start] else { unreachable!() };
            let d = data.get(at..at + n as usize).ok_or_else(corrupt)?;
            if seg_len(seg) != n as u64 {
                return Err(corrupt());
            }
            let mut payload = Vec::new();
            lzvn::write(d, &lzvn::ops(&seg.matches, seg.tail), &mut payload)?;
            f(Block::Lzvn(n, payload), at)?;
            done += 1;
            at += n as usize;
            continue;
        }
        let mut ts = triples(seg);
        for p in &plan.parts[range.clone()] {
            let Part::Lzfse(n, side) = p else { unreachable!() };
            let n = *n as usize;
            let start = at;
            let mut lmd = Vec::with_capacity(n);
            let mut lits = Vec::new();
            let (mut prev, mut z) = (0, side.zd.iter());
            for _ in 0..n {
                let (l, m, d) = ts.next().ok_or_else(corrupt)?;
                let d = match d {
                    Some(d) if d == prev => 0,
                    Some(d) => d,
                    None => *z.next().ok_or_else(corrupt)?,
                };
                if d != 0 {
                    prev = d;
                }
                lits.extend_from_slice(data.get(at..at + l as usize).ok_or_else(corrupt)?);
                at += (l + m) as usize;
                lmd.push([l, m, d]);
            }
            if z.next().is_some() || side.pad.len() > 3 || (lits.len() + side.pad.len()) % 4 != 0 {
                return Err(corrupt());
            }
            lits.extend_from_slice(&side.pad);
            let freqs = match &side.freqs {
                Some(f) => (**f).clone(),
                None => counted(&lits, &lmd),
            };
            let b = Lzfse {
                lits,
                lmd,
                freqs,
                lit_start: side.lit_start,
                lmd_start: side.lmd_start,
                lit_low: side.lit_low,
                lmd_low: side.lmd_low,
            };
            f(Block::Lzfse(Box::new(b)), start)?;
            done += 1;
        }
        if ts.next().is_some() {
            return Err(corrupt());
        }
    }
    if at != data.len() || segs.next().is_some() {
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

/// Writes the stream of `plan` over `data` onto `out`.
pub fn join(data: &[u8], plan: &Plan, out: &mut Vec<u8>) -> Result<()> {
    each_block(data, plan, &mut |b, at| bvx::write_block(&data[at..], &b, out))?;
    out.extend_from_slice(&bvx::END);
    Ok(())
}

/// The shape of each LZFSE block of `plan`: the literals padding its own to a multiple of 4,
/// and its triples of no match.
pub fn shapes(plan: &Plan) -> Result<Vec<(usize, usize)>> {
    let mut out = Vec::new();
    let mut segs = plan.segs.iter();
    for (range, lzvn) in segments(&plan.parts) {
        let seg = segs.next().ok_or_else(corrupt)?;
        if lzvn {
            continue;
        }
        let mut ts = triples(seg);
        for p in &plan.parts[range] {
            let Part::Lzfse(n, _) = p else { unreachable!() };
            let (mut lits, mut none) = (0, 0);
            for _ in 0..*n {
                let t = ts.next().ok_or_else(corrupt)?;
                lits += t.0 as usize;
                none += t.2.is_none() as usize;
            }
            out.push(((4 - lits % 4) % 4, none));
        }
    }
    Ok(out)
}
