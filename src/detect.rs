//! Cheap detection of streams made by zlib, and estimation of zlib's parameters.
//!
//! Structural checks come first, after preflate's estimator (deus-libri/preflate,
//! `preflate_parameter_estimator.cpp` and `preflate_complevel_estimator.cpp`): zlib's dynamic
//! headers are exactly what [`crate::predict::zlib_header`] gives, full blocks hold exactly
//! `lit_bufsize - 1` tokens (which gives memLevel), distances stay within
//! `w_size - MIN_LOOKAHEAD` (a lower bound on windowBits), no match refers to position 0, and
//! lazy levels never emit length-3 matches farther than 4096. The remaining candidates are
//! then ranked by emulating them over a prefix of the stream.

use crate::deflate::{Body, Stream, Token};
use crate::predict::zlib_header;
use crate::zlib::{Emulator, MultiSlow, Params, Strategy};

/// Tokens emulated per candidate in the first round of ranking; each further round covers
/// four times as many among the candidates still tied for the fewest misses.
const TRIAL_TOKENS: usize = 1024;

#[derive(Debug, Clone, Copy)]
pub struct Detection {
    pub params: Params,
    /// Tokens checked in the trial and how many of them were mispredicted.
    pub tokens: usize,
    pub misses: usize,
    /// Whether the trial covered the whole stream.
    pub complete: bool,
}

/// Candidate zlib parameters for `stream`, or nothing if it cannot come from zlib.
pub fn candidates(stream: &Stream) -> Vec<Params> {
    candidates_hinted(stream, &Hints::default())
}

/// What a container says about the encoder that made a stream. zlib and gzip headers carry a
/// coarse compression level and (zlib) the window size; other encoders may fill them in
/// differently, so they only narrow the search and are never relied upon.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Hints {
    /// FLEVEL of a zlib header (0-3).
    pub zlib_flevel: Option<u8>,
    /// XFL of a gzip header.
    pub gzip_xfl: Option<u8>,
    pub wbits: Option<u8>,
}

impl Hints {
    /// From the first two bytes of a zlib stream; no hints if they are not a valid header.
    pub fn zlib_header(header: [u8; 2]) -> Hints {
        let (cmf, flg) = (header[0], header[1]);
        if cmf & 15 != 8 || cmf >> 4 > 7 || !(cmf as u16 * 256 + flg as u16).is_multiple_of(31) {
            return Hints::default();
        }
        Hints {
            zlib_flevel: Some(flg >> 6),
            gzip_xfl: None,
            wbits: Some((cmf >> 4) + 8).filter(|&w| w >= 9),
        }
    }

    /// From the XFL byte of a gzip header.
    pub fn gzip_xfl(xfl: u8) -> Hints {
        Hints {
            gzip_xfl: Some(xfl),
            ..Hints::default()
        }
    }

    /// Whether zlib with `p` would have written these headers.
    pub fn allows(&self, p: &Params) -> bool {
        let special = matches!(
            p.strategy,
            Strategy::HuffmanOnly | Strategy::Rle | Strategy::Fixed
        );
        let flevel = match p.level {
            _ if special || p.level < 2 => 0,
            2..=5 => 1,
            6 => 2,
            _ => 3,
        };
        let xfl = if p.level == 9 {
            2
        } else if special || p.level < 2 {
            4
        } else {
            0
        };
        self.zlib_flevel.is_none_or(|f| f == flevel)
            && self.gzip_xfl.is_none_or(|x| x == xfl)
            && self.wbits.is_none_or(|w| w == p.wbits)
    }
}

/// [`candidates`] consistent with `hints`.
pub fn candidates_hinted(stream: &Stream, hints: &Hints) -> Vec<Params> {
    let mut counts = vec![]; // token counts of non-final Huffman blocks
    let (mut pos, mut max_dist, mut any_match, mut stored_bytes) = (0usize, 0usize, false, 0usize);
    let (mut all_lit, mut all_rle, mut short, mut far3) = (true, true, false, false);
    let mut all_fixed = true;
    for b in &stream.blocks {
        let tokens = match &b.body {
            Body::Stored { len, .. } => {
                pos += *len as usize;
                stored_bytes += *len as usize;
                continue;
            }
            Body::Fixed { tokens } => tokens,
            Body::Dynamic { header, tokens } => {
                all_fixed = false;
                if zlib_header(tokens) != *header {
                    return vec![];
                }
                tokens
            }
        };
        if !b.last {
            counts.push(tokens.len());
        }
        for &t in tokens {
            if let Token::Match { len, dist, alt258 } = t {
                let (len, dist) = (len as usize, dist as usize);
                if alt258 || dist == pos {
                    return vec![]; // zlib never uses either
                }
                any_match = true;
                all_lit = false;
                all_rle &= dist == 1;
                max_dist = max_dist.max(dist);
                short |= len <= 5;
                far3 |= len == 3 && dist > 4096;
            }
            pos += t.span();
        }
    }

    // Without any coded tokens there is nothing to recognize zlib by (and its level 0, which
    // only stores, is not emulated).
    if pos == stored_bytes {
        return vec![];
    }

    // memLevel from full blocks; without any, the default (or 9 if the block is too big).
    let mems: Vec<u8> = match counts.first() {
        Some(&c) => {
            if counts.iter().any(|&x| x != c) {
                return vec![];
            }
            match (1..=9u8).find(|&m| (1usize << (m + 6)) - 1 == c) {
                Some(m) => vec![m],
                None => return vec![],
            }
        }
        None => {
            let last = stream.blocks.last().map_or(0, |b| match &b.body {
                Body::Fixed { tokens } | Body::Dynamic { tokens, .. } => tokens.len(),
                Body::Stored { .. } => 0,
            });
            // The default, or 9 if the block is too big (see `mem_alternatives`).
            vec![if last < (1 << 14) { 8 } else { 9 }]
        }
    };
    // windowBits: the smallest that allows every distance, and the default.
    let Some(wmin) = (9..=15u8).find(|&w| max_dist <= (1usize << w) - 262) else {
        return vec![];
    };
    let wbits: Vec<u8> = match hints.wbits {
        Some(w) if w >= wmin => vec![w],
        _ if wmin == 15 => vec![15],
        _ => vec![15, wmin],
    };

    let mut kinds: Vec<(u8, Strategy)> = vec![];
    if all_lit {
        kinds.push((6, Strategy::HuffmanOnly));
    }
    if any_match && all_rle {
        kinds.push((6, Strategy::Rle));
    }
    // Default level first, so that it wins ties.
    for level in [6, 9, 1, 5, 4, 7, 8, 2, 3] {
        // Lazy levels drop length-3 matches farther than TOO_FAR.
        if level >= 4 && far3 {
            continue;
        }
        // Z_FIXED gives the same tokens; when every block is fixed it is the likelier one.
        if all_fixed {
            kinds.push((level, Strategy::Fixed));
        }
        kinds.push((level, Strategy::Default));
        if level >= 4 && !short && any_match {
            kinds.push((level, Strategy::Filtered));
        }
    }
    let mut out = vec![];
    for &mem in &mems {
        for (i, &wbits) in wbits.iter().enumerate() {
            for &(level, strategy) in &kinds {
                // Huffman-only and RLE tokens do not depend on the window size.
                if i > 0 && matches!(strategy, Strategy::HuffmanOnly | Strategy::Rle) {
                    continue;
                }
                out.push(Params {
                    level,
                    strategy,
                    wbits,
                    mem,
                });
            }
        }
    }
    out.retain(|p| hints.allows(p));
    out
}

/// An emulation of `p` against `stream` that can be continued.
struct Trial<'a> {
    e: Emulator<'a>,
    block: usize,
    token: usize,
    pos: usize,
    d: Detection,
    /// Set once the emulation cannot follow the stream any more.
    lost: bool,
}

impl<'a> Trial<'a> {
    fn new(data: &'a [u8], p: Params) -> Self {
        Trial {
            e: Emulator::new(data, p),
            block: 0,
            token: 0,
            pos: 0,
            d: Detection {
                params: p,
                tokens: 0,
                misses: 0,
                complete: false,
            },
            lost: false,
        }
    }

    /// Continues until `limit` tokens in total, the end of the stream, or more than
    /// `give_up` misses.
    fn run(&mut self, stream: &Stream, limit: usize, give_up: usize) {
        while !self.lost && !self.d.complete && self.d.tokens < limit && self.d.misses <= give_up {
            let Some(b) = stream.blocks.get(self.block) else {
                self.d.complete = true;
                break;
            };
            let ts = match &b.body {
                Body::Stored { len, .. } => {
                    self.pos += *len as usize;
                    self.lost |= !self.e.skip_to(self.pos);
                    self.block += 1;
                    continue;
                }
                Body::Fixed { tokens } | Body::Dynamic { tokens, .. } => tokens,
            };
            let Some(&t) = ts.get(self.token) else {
                self.block += 1;
                self.token = 0;
                continue;
            };
            match self.e.predict() {
                Some((pp, pt)) if pp == self.pos => self.d.misses += (pt != t) as usize,
                _ => {
                    self.lost = true;
                    break;
                }
            }
            self.lost |= !self.e.commit(t);
            self.token += 1;
            self.d.tokens += 1;
            self.pos += t.span();
        }
    }
}

/// Emulates `p` over the first `limit` tokens of `stream`; gives up (`None`) once the misses
/// exceed `give_up`.
pub fn trial(
    stream: &Stream,
    data: &[u8],
    p: Params,
    limit: usize,
    give_up: usize,
) -> Option<Detection> {
    let mut t = Trial::new(data, p);
    t.run(stream, limit, give_up);
    if limit == usize::MAX {
        t.d.complete = !t.lost && t.d.misses <= give_up;
    }
    (!t.lost && t.d.misses <= give_up).then_some(t.d)
}

/// The best zlib parameters for `stream`, if it looks like zlib's output at all. Candidates
/// are emulated over growing prefixes (successive halving): only those tied for the fewest
/// misses go on, until one is left or the whole stream has been covered.
pub fn detect(stream: &Stream, data: &[u8]) -> Option<Detection> {
    detect_among(stream, data, candidates(stream))
}

/// Other memLevels that `stream` allows when it has no full block to tell it by (memLevel
/// only sets the hash size and the block size, so this matters for single-block streams).
pub fn mem_alternatives(stream: &Stream) -> Vec<u8> {
    let huff: Vec<usize> = stream
        .blocks
        .iter()
        .filter_map(|b| match &b.body {
            Body::Fixed { tokens } | Body::Dynamic { tokens, .. } => Some(tokens.len()),
            Body::Stored { .. } => None,
        })
        .collect();
    if huff.len() != 1 {
        return vec![];
    }
    let guess = if huff[0] < (1 << 14) { 8 } else { 9 };
    [9, 7, 6, 5, 4, 3, 2, 1, 8]
        .into_iter()
        .filter(|&m| m != guess && (1usize << (m + 6)) > huff[0])
        .collect()
}

/// [`detect_among`], then, if the best candidate is not exact over the whole stream and its
/// memLevel was only guessed, the same candidate with the other possible memLevels.
pub fn detect_refined(stream: &Stream, data: &[u8], candidates: Vec<Params>) -> Option<Detection> {
    let d = detect_among(stream, data, candidates)?;
    let alts = mem_alternatives(stream);
    if alts.is_empty() {
        return Some(d);
    }
    let mut best = trial(stream, data, d.params, usize::MAX, usize::MAX)?;
    for mem in alts {
        if best.misses == 0 {
            break;
        }
        let p = Params { mem, ..d.params };
        if let Some(t) = trial(stream, data, p, usize::MAX, best.misses.saturating_sub(1)) {
            best = t;
        }
    }
    Some(best)
}

/// The usual settings among `candidates`: levels 6 (zlib's default), 9 and 1, and the
/// Huffman-only and RLE strategies, with the most likely memLevel (the first candidate's).
pub fn common(candidates: Vec<Params>) -> Vec<Params> {
    let mem = candidates.first().map(|p| p.mem);
    candidates
        .into_iter()
        .filter(|p| Some(p.mem) == mem)
        .filter(|p| match p.strategy {
            Strategy::HuffmanOnly | Strategy::Rle => true,
            Strategy::Default | Strategy::Fixed => matches!(p.level, 1 | 6 | 9),
            Strategy::Filtered => false,
        })
        .collect()
}

/// [`detect`] restricted to `candidates`.
/// Emulation units for `candidates`: deflate_slow candidates sharing windowBits and memLevel
/// together (see `MultiSlow`), unless stored blocks would make their states diverge.
fn units<'a>(stream: &Stream, data: &'a [u8], candidates: Vec<Params>) -> Vec<Unit<'a>> {
    let shareable = |p: &Params| {
        p.level >= 4
            && matches!(
                p.strategy,
                Strategy::Default | Strategy::Filtered | Strategy::Fixed
            )
    };
    let has_stored = stream
        .blocks
        .iter()
        .any(|b| matches!(b.body, Body::Stored { .. }));
    let mut units: Vec<Unit> = vec![];
    // (windowBits, memLevel) -> (candidate index, parameters)
    type Group = ((u8, u8), Vec<(usize, Params)>);
    let mut groups: Vec<Group> = vec![];
    for (i, p) in candidates.into_iter().enumerate() {
        if shareable(&p) && !has_stored {
            match groups.iter_mut().find(|(k, _)| *k == (p.wbits, p.mem)) {
                Some((_, g)) => g.push((i, p)),
                None => groups.push(((p.wbits, p.mem), vec![(i, p)])),
            }
        } else {
            units.push(Unit::Single(i, Trial::new(data, p)));
        }
    }
    for (_, g) in groups {
        units.push(Unit::Multi(MultiRun::new(data, g)));
    }
    units
}

/// The first of `candidates` (in their order) that predicts the first [`TRIAL_TOKENS`]
/// tokens without a miss: a cheap guess, to be verified by coding the whole stream.
pub fn first_exact(stream: &Stream, data: &[u8], candidates: Vec<Params>) -> Option<Detection> {
    let mut found: Vec<(usize, Detection)> = vec![];
    for mut u in units(stream, data, candidates) {
        u.run(stream, TRIAL_TOKENS, 0);
        found.extend(u.detections().filter(|(_, d)| d.misses == 0));
    }
    found.into_iter().min_by_key(|&(i, _)| i).map(|(_, d)| d)
}

pub fn detect_among(stream: &Stream, data: &[u8], candidates: Vec<Params>) -> Option<Detection> {
    let mut units = units(stream, data, candidates);
    let mut limit = TRIAL_TOKENS;
    loop {
        let mut best = usize::MAX;
        for u in &mut units {
            u.run(stream, limit, best);
            best = best.min(u.best());
        }
        for u in &mut units {
            u.keep(best);
        }
        units.retain(Unit::alive);
        // Survivors by order of preference (the candidates' order).
        let mut alive: Vec<(usize, Detection)> = units.iter().flat_map(Unit::detections).collect();
        alive.sort_by_key(|&(i, _)| i);
        if alive.len() <= 1 || alive.iter().all(|(_, d)| d.complete) {
            // Ties that survive the whole stream are equivalent; the first is preferred.
            return alive.first().map(|&(_, d)| d);
        }
        limit = limit.saturating_mul(4);
    }
}

/// A resumable [`MultiSlow`] run against a stream; lane `k` is candidate `ids[k]`.
struct MultiRun<'a> {
    m: MultiSlow<'a>,
    ids: Vec<usize>,
    block: usize,
    token: usize,
    pos: usize,
    tokens: usize,
    complete: bool,
    lost: bool,
}

impl<'a> MultiRun<'a> {
    fn new(data: &'a [u8], lanes: Vec<(usize, Params)>) -> Self {
        let params: Vec<Params> = lanes.iter().map(|&(_, p)| p).collect();
        MultiRun {
            m: MultiSlow::new(data, &params),
            ids: lanes.iter().map(|&(i, _)| i).collect(),
            block: 0,
            token: 0,
            pos: 0,
            tokens: 0,
            complete: false,
            lost: false,
        }
    }

    fn run(&mut self, stream: &Stream, limit: usize, give_up: usize) {
        while !self.lost && !self.complete && self.tokens < limit && self.m.any_alive() {
            let Some(b) = stream.blocks.get(self.block) else {
                self.complete = true;
                break;
            };
            let ts = match &b.body {
                Body::Fixed { tokens } | Body::Dynamic { tokens, .. } => tokens,
                Body::Stored { .. } => {
                    self.lost = true;
                    break;
                }
            };
            let Some(&t) = ts.get(self.token) else {
                self.block += 1;
                self.token = 0;
                continue;
            };
            if self.m.predict() != Some(self.pos) || !self.m.commit(t) {
                self.lost = true;
                break;
            }
            self.token += 1;
            self.tokens += 1;
            self.pos += t.span();
            self.kill_above(give_up);
        }
    }

    fn kill_above(&mut self, give_up: usize) {
        let dead: Vec<usize> = self
            .m
            .lanes()
            .filter(|&(_, _, x)| x > give_up)
            .map(|(k, ..)| k)
            .collect();
        for k in dead {
            self.m.kill(k);
        }
    }
}

enum Unit<'a> {
    Single(usize, Trial<'a>),
    Multi(MultiRun<'a>),
}

impl Unit<'_> {
    fn run(&mut self, stream: &Stream, limit: usize, give_up: usize) {
        match self {
            Unit::Single(_, t) => t.run(stream, limit, give_up),
            Unit::Multi(m) => m.run(stream, limit, give_up),
        }
    }

    /// Fewest misses among the live candidates.
    fn best(&self) -> usize {
        self.detections()
            .map(|(_, d)| d.misses)
            .min()
            .unwrap_or(usize::MAX)
    }

    /// Keeps only the candidates with `best` misses.
    fn keep(&mut self, best: usize) {
        match self {
            Unit::Single(_, t) => t.lost |= t.d.misses != best,
            Unit::Multi(m) => {
                let dead: Vec<usize> =
                    m.m.lanes()
                        .filter(|&(_, _, x)| x != best)
                        .map(|(k, ..)| k)
                        .collect();
                for k in dead {
                    m.m.kill(k);
                }
            }
        }
    }

    fn alive(&self) -> bool {
        self.detections().next().is_some()
    }

    fn detections(&self) -> Box<dyn Iterator<Item = (usize, Detection)> + '_> {
        match self {
            Unit::Single(i, t) => Box::new((!t.lost).then_some((*i, t.d)).into_iter()),
            Unit::Multi(m) => Box::new(m.m.lanes().filter(|_| !m.lost).map(
                move |(k, params, misses)| {
                    (
                        m.ids[k],
                        Detection {
                            params,
                            tokens: m.tokens,
                            misses,
                            complete: m.complete,
                        },
                    )
                },
            )),
        }
    }
}

/// Emulates all of `params` (deflate_slow levels sharing windowBits and memLevel) at once over
/// the first `limit` tokens of `stream`, dropping each once it exceeds `give_up` misses. Not
/// for streams with stored blocks (whose tokens the lanes would each have to guess).
pub fn multi_trial(
    stream: &Stream,
    data: &[u8],
    params: &[Params],
    limit: usize,
    give_up: usize,
) -> Vec<Detection> {
    let mut m = MultiSlow::new(data, params);
    let (mut pos, mut tokens) = (0, 0);
    let mut complete = true;
    'blocks: for b in &stream.blocks {
        let ts = match &b.body {
            Body::Stored { .. } => return vec![],
            Body::Fixed { tokens } | Body::Dynamic { tokens, .. } => tokens,
        };
        for &t in ts {
            if tokens == limit {
                complete = false;
                break 'blocks;
            }
            if m.predict() != Some(pos) || !m.commit(t) {
                return vec![];
            }
            tokens += 1;
            pos += t.span();
            let dead: Vec<usize> = m
                .lanes()
                .filter(|&(_, _, x)| x > give_up)
                .map(|(i, ..)| i)
                .collect();
            for i in dead {
                m.kill(i);
            }
            if !m.any_alive() {
                return vec![];
            }
        }
    }
    m.lanes()
        .map(|(_, params, misses)| Detection {
            params,
            tokens,
            misses,
            complete,
        })
        .collect()
}
