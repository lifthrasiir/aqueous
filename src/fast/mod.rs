//! A speed-oriented alternative to the decision stream: tokens are predicted by a fixed rule
//! (no context modelling, no encoder emulation), and only the mispredictions are stored,
//! with static codes.
//!
//! ```no_run
//! # let raw: Vec<u8> = vec![];
//! let (data, meta) = flate_recons::fast::deconstruct(&raw)?;
//! assert_eq!(flate_recons::fast::reconstruct(&data, &meta)?, raw);
//! # Ok::<(), flate_recons::Error>(())
//! ```
//!
//! The prediction at a position depends on nothing but the data, so the tokens are cut into
//! segments that are coded and rebuilt in parallel. Rebuilding a correctly predicted token
//! costs one match search and nothing else: the corrections are a sparse list read only at
//! the misses.
//!
//! Metadata layout: a version byte, the lengths of the two bit streams that follow (main and
//! headers), then each segment's corrections, byte aligned.
//! - main: the rule; the blocks (type, token count or stored length); padding and trailing
//!   bytes; the correction codes; per segment its token count, misses, start and size.
//! - headers: each dynamic block's Huffman header, against zlib's trees for its tokens.

pub mod code;
pub mod header;
pub mod inflate;
#[doc(hidden)]
pub mod table;
pub mod tokens;
#[doc(hidden)]
pub mod trees;
pub mod write;

use crate::bits::{BitReader, BitWriter};
use crate::deflate::{self, Block, Body, Stream, Token};
use crate::{Error, Result};
use code::{get_usize, get_varint, put_gamma, put_varint};
pub use tokens::Rule;
use tokens::{FIELDS, Range};

const VERSION: u8 = 2;

#[derive(Debug, Clone)]
pub struct Options {
    /// Rules to choose from.
    pub rules: Vec<Rule>,
    /// The rule (index in `rules`) for streams by their [fingerprint](fingerprint); those
    /// without one have the rules tried on a prefix.
    pub map: Map,
    /// Threads for coding segments.
    pub threads: usize,
    /// Segments cover about this many bytes of data.
    pub segment: usize,
    /// Rules are compared on about this many bytes of data when the fingerprint does not
    /// settle the choice.
    pub prefix: usize,
    /// Check that the stream is written back exactly from what was parsed (a safeguard
    /// against parser or writer bugs, which costs a full write).
    pub verify: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            rules: catalog(),
            map: builtin_map().clone(),
            threads: crate::default_threads(),
            segment: 1 << 20,
            prefix: 4 << 10,
            verify: false,
        }
    }
}

/// Bits written so far.
fn w_bits(w: &BitWriter) -> usize {
    w.bit_len()
}

/// The built-in default models of the correction fields (`model/fast-models.txt`): one
/// line per field, the rounded frequencies of its symbols.
pub fn default_models() -> &'static [code::Model; FIELDS] {
    static M: std::sync::OnceLock<[code::Model; FIELDS]> = std::sync::OnceLock::new();
    M.get_or_init(|| {
        let mut lines = include_str!("../../model/fast-models.txt")
            .lines()
            .filter(|l| !l.trim().is_empty() && !l.starts_with('#'));
        std::array::from_fn(|_| {
            lines
                .next()
                .and_then(code::Model::parse)
                .unwrap_or_else(code::Model::uniform)
        })
    })
}

/// The built-in rule catalog.
pub fn catalog() -> Vec<Rule> {
    builtin().to_vec()
}

fn builtin() -> &'static [Rule] {
    static C: std::sync::OnceLock<Vec<Rule>> = std::sync::OnceLock::new();
    C.get_or_init(|| {
        parse_catalog(include_str!("../../model/rules.txt"))
            .expect("built-in rules are valid")
            .0
    })
}

fn builtin_map() -> &'static Map {
    static M: std::sync::OnceLock<Map> = std::sync::OnceLock::new();
    M.get_or_init(|| {
        parse_catalog(include_str!("../../model/rules.txt"))
            .unwrap()
            .1
    })
}

/// The rules (indices in a catalog) for fingerprints: one, or two to try on a short prefix.
pub type Map = std::collections::HashMap<u64, Vec<u16>>;

/// A catalog's text: one rule per line, and lines `map FINGERPRINT INDEX...` (hexadecimal
/// fingerprint, the rules' indices).
pub fn parse_catalog(text: &str) -> Result<(Vec<Rule>, Map)> {
    let mut rules = vec![];
    let mut map = std::collections::HashMap::new();
    for l in text.lines().map(str::trim) {
        if l.is_empty() || l.starts_with('#') {
            continue;
        }
        if let Some(m) = l.strip_prefix("map ") {
            let bad = || Error::Param(format!("bad map line {l:?}"));
            let mut it = m.split(' ');
            let k = u64::from_str_radix(it.next().ok_or_else(bad)?, 16).map_err(|_| bad())?;
            let v: Vec<u16> = it
                .map(|v| v.parse().map_err(|_| bad()))
                .collect::<Result<_>>()?;
            if v.is_empty() {
                return Err(bad());
            }
            map.insert(k, v);
        } else {
            rules.push(l.parse()?);
        }
    }
    Ok((rules, map))
}

/// The dynamic headers coded, and the first one's recipe.
fn headers(f: &Flat) -> (Vec<u8>, Option<u32>) {
    let mut w = BitWriter::new();
    let mut recipe = header::Recipe::default();
    let mut first = None;
    for (b, (lf, df)) in f.stream.blocks.iter().zip(&f.stats.freqs) {
        if let Body::Dynamic { header, .. } = &b.body {
            header::write(&mut w, lf, df, header, &mut recipe);
            first.get_or_insert(recipe.key());
        }
    }
    w.bits(w.pad_len(), 0);
    (w.finish(), first)
}

/// Cheap features of a stream that tell encoders apart, as keys: a fine one and a coarse
/// one (for streams the fine one was not seen for). They are the shortest match length
/// in common use, the farthest distance, whether short matches go far, the token count of
/// full blocks, the block types used, and how the first dynamic header was made (`recipe`,
/// see [`header::Recipe::key`]).
fn fingerprint(f: &Flat, recipe: u32) -> (u64, u64) {
    let (mut lf, mut df) = ([0u64; 3], [0u64; 30]);
    for (l, d) in &f.stats.freqs {
        lf.iter_mut()
            .zip(&l[257..])
            .for_each(|(a, &b)| *a += b as u64);
        df.iter_mut().zip(d).for_each(|(a, &b)| *a += b as u64);
    }
    let n: u64 = df.iter().sum();
    let common = |c: u64| c * 100 > n;
    // Length codes 0..2 are lengths 3..5, distance code 0 is distance 1.
    let min_len = if n == 0 {
        0
    } else if df[0] == n {
        1
    } else if common(lf[0]) {
        3
    } else if common(lf[1]) {
        4
    } else {
        5
    };
    let window = [
        256, 1024, 2048, 4096, 8192, 16384, 32506, 32510, 32767, 32768,
    ]
    .iter()
    .position(|&w| f.stats.max_dist as usize <= w)
    .unwrap_or(10);
    // The token count of the first block, if the next has as many.
    let counts: Vec<usize> = f.starts.windows(2).map(|s| s[1] - s[0]).collect();
    let coded: Vec<usize> = f
        .stream
        .blocks
        .iter()
        .zip(&counts)
        .filter(|(b, _)| !matches!(b.body, Body::Stored { .. }))
        .map(|(_, &c)| c)
        .collect();
    let full = if coded.len() >= 3 && coded[0] == coded[1] {
        coded[0] as u64
    } else {
        0
    };
    let types = f
        .stream
        .blocks
        .iter()
        .fold(0u64, |a, b| a | 1 << b.body.btype());
    let mix = |v: &[u64]| {
        v.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &x| {
            (h ^ x).wrapping_mul(0x100_0000_01b3)
        })
    };
    let far = f.stats.far as u64;
    let coarse = mix(&[1, min_len, window as u64, far]);
    let fine = mix(&[2, min_len, window as u64, far, full, types, recipe as u64]);
    (fine, coarse)
}

pub fn deconstruct(raw: &[u8]) -> Result<(Vec<u8>, Vec<u8>)> {
    deconstruct_with(raw, &Options::default())
}

use inflate::Flat;

impl Flat {
    fn new(raw: &[u8], segment: usize, threads: usize, verify: bool) -> Result<Flat> {
        let f = inflate::parse_segments(raw, segment)?;
        if verify {
            // The writer must round-trip what the parser saw, or nothing downstream can.
            let mut stream = f.stream.clone();
            for (b, s) in stream.blocks.iter_mut().zip(f.starts.windows(2)) {
                if let Body::Fixed { tokens } | Body::Dynamic { tokens, .. } = &mut b.body {
                    *tokens = f.tokens[s[0]..s[1]].to_vec();
                }
            }
            if deflate::write_threads(&stream, &f.data, threads)? != raw {
                return Err(Error::Inconsistent);
            }
        }
        Ok(f)
    }
}

/// One segment: a token range and its corrections.
struct Seg {
    first: usize,
    count: usize,
    pos: usize,
    rec: tokens::Recorder,
}

pub fn deconstruct_with(raw: &[u8], opts: &Options) -> Result<(Vec<u8>, Vec<u8>)> {
    deconstruct_report(raw, opts).map(|(d, m, _)| (d, m))
}

/// What the metadata of a stream consists of.
#[derive(Debug, Clone, Default)]
pub struct Report {
    pub rule: Rule,
    pub tokens: usize,
    pub misses: usize,
    /// Bytes of the block layout, codes and segment directory (the rule included).
    pub main: usize,
    pub headers: usize,
    pub corrections: usize,
    /// Bytes of the corrections' fields (gaps, kinds, lengths, distances; see [`tokens::FIELDS`]), as
    /// estimated with their codes.
    pub fields: [f64; FIELDS],
    /// Bytes they take with the codes used.
    pub coded: [f64; FIELDS],
    /// Bytes of the fields' frequency tables (part of `main`).
    pub tables: usize,
    /// Symbol counts of each field.
    pub counts: Vec<Vec<u32>>,
    /// The stream's fingerprint (fine, coarse).
    pub fingerprint: (u64, u64),
    /// Seconds spent parsing, choosing the rule, coding the tokens, coding the headers, and
    /// on the rest.
    pub times: [f64; 5],
}

/// Like [`deconstruct_with`], also describing the metadata.
pub fn deconstruct_report(raw: &[u8], opts: &Options) -> Result<(Vec<u8>, Vec<u8>, Report)> {
    let threads = opts.threads.max(1);
    let t0 = std::time::Instant::now();
    let f = Flat::new(raw, opts.segment, threads, opts.verify)?;
    let t1 = std::time::Instant::now();
    let n = f.data.len();

    let t4 = std::time::Instant::now();
    let (headers, first_recipe) = headers(&f);
    let fp = fingerprint(&f, first_recipe.unwrap_or(u32::MAX));
    let (cuts, cut_pos) = (&f.stats.cuts, &f.stats.cut_pos);
    let ranges: Vec<(usize, usize)> = cuts.windows(2).map(|c| (c[0], c[1] - c[0])).collect();
    let range_pos = |i: usize| -> usize { cut_pos[i] };
    let range_end = |i: usize| -> usize { ranges.get(i + 1).map_or(n, |_| range_pos(i + 1)) };
    let t5 = std::time::Instant::now();
    let rule = choose_rule(&f, opts, fp)?;
    let t2 = std::time::Instant::now();
    let segs = par_map(ranges.len(), threads, |i| -> Result<Seg> {
        let (first, count) = ranges[i];
        let range = Range {
            first,
            count,
            pos: range_pos(i),
            end: range_end(i),
            skips: &f.skips,
        };
        let mut rec = tokens::Recorder::default();
        let actual = &f.tokens[first..first + count];
        tokens::code_range(&mut rec, &f.data, rule, &range, Some(actual), None)?;
        Ok(Seg {
            first,
            count,
            pos: range.pos,
            rec,
        })
    })
    .into_iter()
    .collect::<Result<Vec<Seg>>>()?;

    let t3 = std::time::Instant::now();
    let mut counts: [code::Counts; FIELDS] = Default::default();
    for s in &segs {
        for (a, b) in counts.iter_mut().zip(&s.rec.counts) {
            a.merge(b);
        }
    }
    let mut w = BitWriter::new();
    // The rule as its index in the built-in catalog, or in full.
    match builtin().iter().position(|r| *r == rule) {
        Some(k) => {
            w.bits(1, 1);
            put_gamma(&mut w, k as u64);
        }
        None => {
            w.bits(1, 0);
            rule.write(&mut w);
        }
    }
    write_blocks(&mut w, &f);
    let defaults = default_models();
    let before = w_bits(&w);
    let models: [code::Model; FIELDS] =
        std::array::from_fn(|k| code::Model::choose(&counts[k], &defaults[k], &mut w));
    let tables = (w_bits(&w) - before).div_ceil(8);
    let bodies = par_map(segs.len(), threads, |i| segs[i].rec.write(&models));
    put_gamma(&mut w, segs.len() as u64);
    for (s, body) in segs.iter().zip(&bodies) {
        put_gamma(&mut w, s.count as u64);
        put_gamma(&mut w, s.rec.misses[0] as u64);
        put_gamma(&mut w, s.rec.misses[1] as u64);
        if s.first > 0 {
            put_gamma(&mut w, s.pos as u64);
        }
        put_gamma(&mut w, body.len() as u64);
    }
    w.bits(w.pad_len(), 0);
    let main = w.finish();

    let secs = |a: std::time::Instant, b: std::time::Instant| (b - a).as_secs_f64();
    let report = Report {
        times: [
            secs(t0, t1),
            secs(t5, t2),
            secs(t2, t3),
            secs(t4, t5),
            secs(t3, std::time::Instant::now()),
        ],
        rule,
        tokens: f.tokens.len(),
        misses: segs.iter().map(|s| s.rec.misses[0] + s.rec.misses[1]).sum(),
        main: main.len(),
        headers: headers.len(),
        corrections: bodies.iter().map(Vec::len).sum(),
        fields: std::array::from_fn(|k| counts[k].cost() / 8.0),
        coded: std::array::from_fn(|k| models[k].bits(&counts[k]) / 8.0),
        tables,
        counts: counts.iter().map(|c| c.0.to_vec()).collect(),
        fingerprint: fp,
    };
    let mut meta = vec![VERSION];
    put_varint(&mut meta, main.len() as u64);
    put_varint(&mut meta, headers.len() as u64);
    meta.extend(main);
    meta.extend(headers);
    for b in bodies {
        meta.extend(b);
    }
    Ok((f.data, meta, report))
}

/// The cheapest of the candidate rules on a prefix of the stream.
fn choose_rule(f: &Flat, opts: &Options, fp: (u64, u64)) -> Result<Rule> {
    let rules = &opts.rules;
    if rules.len() <= 1 {
        return Ok(rules.first().copied().unwrap_or_default());
    }
    for k in [fp.0, fp.1] {
        let Some(cands) = opts.map.get(&k) else {
            continue;
        };
        let cands: Vec<Rule> = cands
            .iter()
            .filter_map(|&i| rules.get(i as usize))
            .copied()
            .collect();
        match cands[..] {
            [] => continue,
            [r] => return Ok(r),
            _ => {
                // A short trial between them.
                let mut p = Prepared::from_flat(f, opts.prefix);
                let mut best = (f64::INFINITY, cands[0]);
                for &r in &cands {
                    if let Some(c) = p.cost_within(r, best.0)?
                        && c < best.0
                    {
                        best = (c, r);
                    }
                }
                return Ok(best.1);
            }
        }
    }
    // An unknown fingerprint: every rule on a short prefix.
    let mut p = Prepared::from_flat(f, opts.prefix);
    // Rules the tokens contradict are not the encoder's.
    let fit: Vec<Rule> = rules.iter().copied().filter(|r| p.consistent(r)).collect();
    let rules = if fit.is_empty() { rules } else { &fit };
    let mut best = (f64::INFINITY, rules[0]);
    for &r in rules {
        if let Some(c) = p.cost_within(r, best.0)?
            && c < best.0
        {
            best = (c, r);
        }
    }
    Ok(best.1)
}

/// Bits a miss is assumed to cost at least, to give up on a rule early.
const MIN_MISS_BITS: f64 = 4.0;

/// The first part of a stream, ready for costing rules on it.
pub struct Prepared {
    data: Vec<u8>,
    tokens: Vec<Token>,
    skips: Vec<(usize, usize)>,
}

impl Prepared {
    /// The first `prefix` bytes of data of a raw stream.
    pub fn new(raw: &[u8], prefix: usize) -> Result<Prepared> {
        let f = Flat::new(raw, usize::MAX, 1, false)?;
        Ok(Prepared::from_flat(&f, prefix))
    }

    fn from_flat(f: &Flat, prefix: usize) -> Prepared {
        // The tokens starting before `prefix`, and where the next one starts.
        let (mut count, mut p) = (0, 0);
        let mut skips = f.skips.iter().peekable();
        while count < f.tokens.len() {
            while let Some(&&(at, len)) = skips.peek()
                && at == count
            {
                p += len;
                skips.next();
            }
            if p >= prefix {
                break;
            }
            p += f.tokens[count].span();
            count += 1;
        }
        let lim = (p + crate::matchfind::MAX_MATCH).min(f.data.len());
        let data = f.data[..lim].to_vec();
        Prepared {
            tokens: f.tokens[..count].to_vec(),
            skips: f.skips.iter().copied().filter(|s| s.0 < count).collect(),
            data,
        }
    }

    /// Whether the tokens could come from `rule`, as far as cheap statistics tell: few of
    /// the matches contradict it (encoders' hash collisions make a few odd ones).
    pub fn consistent(&self, r: &Rule) -> bool {
        use tokens::Kind;
        let (mut n, mut bad) = (0usize, 0usize);
        for t in &self.tokens {
            if let Token::Match { len, dist, .. } = *t {
                let (l, d) = (len as usize, dist as usize);
                n += 1;
                bad += match r.kind {
                    Kind::Literals => true,
                    Kind::Runs => d != 1,
                    Kind::Search => {
                        l < r.min_len as usize
                            || d > r.window as usize
                            || (l <= r.far_len as usize && d > r.far_dist as usize)
                    }
                } as usize;
            }
        }
        bad * 100 <= n
    }

    /// [`Prepared::cost`], or `None` once it surely exceeds `budget`.
    pub fn cost_within(&mut self, rule: Rule, budget: f64) -> Result<Option<f64>> {
        let limit = if budget.is_finite() {
            (budget / MIN_MISS_BITS) as usize
        } else {
            usize::MAX
        };
        match self.cost_limited(rule, limit) {
            Err(Error::Corrupt) => Ok(None),
            r => r.map(Some),
        }
    }

    /// Bytes of data covered.
    pub fn bytes(&self) -> usize {
        self.data.len()
    }

    /// Estimated cost in bits of the corrections under `rule`.
    pub fn cost(&mut self, rule: Rule) -> Result<f64> {
        self.cost_limited(rule, usize::MAX)
    }

    fn cost_limited(&mut self, rule: Rule, limit: usize) -> Result<f64> {
        let range = Range {
            first: 0,
            count: self.tokens.len(),
            pos: 0,
            end: self.data.len(),
            skips: &self.skips,
        };
        let mut rec = tokens::Recorder::default();
        rec.limit = limit;
        rec.count_only = true;
        let r = tokens::code_range(&mut rec, &self.data, rule, &range, Some(&self.tokens), None);
        r?;
        Ok(rec.cost())
    }
}

/// A stream's [fingerprint] and the estimated cost in bits of each rule over the first
/// `prefix` bytes of its data, for deriving rule catalogs.
pub fn analyze(raw: &[u8], rules: &[Rule], prefix: usize) -> Result<((u64, u64), Vec<f64>, usize)> {
    let f = Flat::new(raw, usize::MAX, 1, false)?;
    let fp = fingerprint(&f, headers(&f).1.unwrap_or(u32::MAX));
    let mut p = Prepared::from_flat(&f, prefix);
    let costs = rules.iter().map(|&r| p.cost(r)).collect::<Result<_>>()?;
    Ok((fp, costs, p.bytes()))
}

/// Estimated cost in bits of each rule over the first `prefix` bytes of data of a raw stream,
/// for deriving rule catalogs.
pub fn rule_costs(raw: &[u8], rules: &[Rule], prefix: usize) -> Result<Vec<f64>> {
    let mut p = Prepared::new(raw, prefix)?;
    rules.iter().map(|&r| p.cost(r)).collect()
}

/// Blocks: type, then the token count (same as the previous block's, the remaining tokens,
/// or explicit) or the stored length and padding; then the tail padding and trailing bytes.
fn write_blocks(w: &mut BitWriter, f: &Flat) {
    let blocks = &f.stream.blocks;
    put_gamma(w, blocks.len() as u64);
    let last_coded = blocks
        .iter()
        .rposition(|b| !matches!(b.body, Body::Stored { .. }));
    let (mut prev_count, mut prev_stored) = (None, None);
    for (i, b) in blocks.iter().enumerate() {
        w.bits(2, b.body.btype() as u32);
        match b.body {
            Body::Stored { pad, len } => {
                put_pad(w, pad);
                if Some(len) == prev_stored {
                    w.bits(2, 0);
                } else if len == u16::MAX {
                    w.bits(2, 1);
                } else {
                    w.bits(2, 2);
                    w.bits(16, len as u32);
                }
                prev_stored = Some(len);
            }
            _ => {
                let count = f.starts[i + 1] - f.starts[i];
                if prev_count == Some(count) {
                    w.bits(1, 1);
                } else {
                    w.bits(1, 0);
                    // The last coded block can take the remaining tokens.
                    if Some(i) == last_coded {
                        w.bits(1, 1);
                    } else {
                        if last_coded.is_some_and(|l| i < l) {
                            w.bits(1, 0);
                        }
                        put_gamma(w, count as u64);
                    }
                }
                prev_count = Some(count);
            }
        }
    }
    put_pad(w, f.stream.tail_pad);
    put_gamma(w, f.stream.trailing.len() as u64);
    for &b in &f.stream.trailing {
        w.bits(8, b as u32);
    }
}

fn put_pad(w: &mut BitWriter, pad: u8) {
    if pad == 0 {
        w.bits(1, 0);
    } else {
        w.bits(1, 1);
        w.bits(7, pad as u32);
    }
}

fn get_pad(r: &mut BitReader) -> Result<u8> {
    Ok(if r.bits(1)? == 1 { r.bits(7)? as u8 } else { 0 })
}

/// What [`write_blocks`] says of a block.
enum Shape {
    Stored {
        pad: u8,
        len: u16,
    },
    /// Fixed (1) or dynamic (2), with its token count (`None`: the remaining tokens).
    Coded {
        btype: u8,
        count: Option<usize>,
    },
}

struct Layout {
    blocks: Vec<Shape>,
    tail_pad: u8,
    trailing: Vec<u8>,
}

fn read_blocks(r: &mut BitReader, n: usize) -> Result<Layout> {
    // Every block but the last covers at least a byte (stored blocks aside, which are bounded
    // by the metadata's length anyway).
    let nblocks = get_usize(r, n + (r.bit_pos() + 1) * 8 + (1 << 20))?;
    let mut blocks = Vec::with_capacity(nblocks.min(1 << 16));
    let (mut prev_count, mut prev_stored) = (None, None);
    let mut rest_used = false;
    for _ in 0..nblocks {
        let btype = r.bits(2)? as u8;
        blocks.push(match btype {
            0 => {
                let pad = get_pad(r)?;
                let len = match r.bits(2)? {
                    0 => prev_stored.ok_or(Error::Corrupt)?,
                    1 => u16::MAX,
                    2 => r.bits(16)? as u16,
                    _ => return Err(Error::Corrupt),
                };
                prev_stored = Some(len);
                Shape::Stored { pad, len }
            }
            1 | 2 => {
                let count = if r.bits(1)? == 1 {
                    Some(prev_count.ok_or(Error::Corrupt)?)
                } else if !rest_used && r.bits(1)? == 1 {
                    rest_used = true;
                    None
                } else {
                    Some(get_usize(r, n)?)
                };
                if count.is_some() {
                    prev_count = count;
                }
                Shape::Coded { btype, count }
            }
            _ => return Err(Error::Corrupt),
        });
    }
    let tail_pad = get_pad(r)?;
    let ntrail = get_usize(r, 1 << 30)?;
    let mut trailing = Vec::with_capacity(ntrail.min(1 << 20));
    for _ in 0..ntrail {
        trailing.push(r.bits(8)? as u8);
    }
    Ok(Layout {
        blocks,
        tail_pad,
        trailing,
    })
}

pub fn reconstruct(data: &[u8], meta: &[u8]) -> Result<Vec<u8>> {
    reconstruct_with_threads(data, meta, crate::default_threads())
}

pub fn reconstruct_with_threads(data: &[u8], meta: &[u8], threads: usize) -> Result<Vec<u8>> {
    let threads = threads.max(1);
    let n = data.len();
    if meta.first() != Some(&VERSION) {
        return Err(Error::Corrupt);
    }
    let mut at = 1;
    let main_len = get_varint(meta, &mut at)? as usize;
    let hdr_len = get_varint(meta, &mut at)? as usize;
    let main = meta.get(at..at + main_len).ok_or(Error::Corrupt)?;
    at += main_len;
    let hdrs = meta.get(at..at + hdr_len).ok_or(Error::Corrupt)?;
    at += hdr_len;

    let r = &mut BitReader::new(main);
    let rule = if r.bits(1)? == 1 {
        *builtin()
            .get(get_usize(r, 1 << 16)?)
            .ok_or(Error::Corrupt)?
    } else {
        Rule::read(r)?
    };
    let layout = read_blocks(r, n)?;
    let defaults = default_models();
    let mut models = Vec::with_capacity(FIELDS);
    for d in defaults {
        models.push(code::Model::read(r, d)?);
    }
    let models: [code::Model; FIELDS] = models.try_into().map_err(|_| Error::Corrupt)?;
    let nseg = get_usize(r, n + 1)?;
    // (first token, count, misses, position, body)
    let mut segs = Vec::with_capacity(nseg);
    let mut first = 0;
    for i in 0..nseg {
        let count = get_usize(r, n)?;
        let misses = [get_usize(r, count)?, get_usize(r, count)?];
        let pos = if i > 0 { get_usize(r, n)? } else { 0 };
        let len = get_usize(r, meta.len())?;
        let body = meta.get(at..at + len).ok_or(Error::Corrupt)?;
        at += len;
        segs.push((first, count, misses, pos, body));
        first += count;
    }
    let total = first;
    if at != meta.len() || segs.windows(2).any(|s| s[0].3 > s[1].3) {
        return Err(Error::Corrupt);
    }

    // Resolve the token counts and where stored blocks interrupt the tokens.
    let fixed: usize = layout
        .blocks
        .iter()
        .map(|b| match b {
            Shape::Coded { count: Some(c), .. } => *c,
            _ => 0,
        })
        .sum();
    let rest = total.checked_sub(fixed).ok_or(Error::Corrupt)?;
    let mut counts = Vec::with_capacity(layout.blocks.len());
    let mut skips = vec![];
    let mut k = 0;
    let mut rest_seen = false;
    for b in &layout.blocks {
        let c = match *b {
            Shape::Stored { len, .. } => {
                skips.push((k, len as usize));
                0
            }
            Shape::Coded { count, .. } => {
                rest_seen |= count.is_none();
                count.unwrap_or(rest)
            }
        };
        k += c;
        counts.push(c);
    }
    if k != total || (rest > 0 && !rest_seen) {
        return Err(Error::Corrupt);
    }

    let decoded = par_map(segs.len(), threads, |i| -> Result<(Vec<Token>, usize)> {
        let (first, count, misses, pos, body) = segs[i];
        let end = segs.get(i + 1).map_or(n, |s| s.3);
        let range = Range {
            first,
            count,
            pos,
            end,
            skips: &skips,
        };
        let mut player = tokens::Player::new(body, &models, misses)?;
        let mut out = Vec::with_capacity(count);
        let p = tokens::code_range(&mut player, data, rule, &range, None, Some(&mut out))?;
        if !player.done() {
            return Err(Error::Corrupt);
        }
        Ok((out, p))
    });
    let mut toks = Vec::with_capacity(segs.len());
    let mut end = 0;
    for (i, r) in decoded.into_iter().enumerate() {
        let (t, p) = r?;
        if p != segs.get(i + 1).map_or(p, |s| s.3) {
            return Err(Error::Corrupt);
        }
        toks.push(t);
        end = p;
    }
    // Stored blocks after the last token cover the rest of the data.
    end += skips
        .iter()
        .filter(|s| s.0 == total)
        .map(|s| s.1)
        .sum::<usize>();
    if end != n {
        return Err(Error::Corrupt);
    }
    let tokens: Vec<Token> = toks.concat();
    drop(toks);

    // The blocks, with their headers.
    let hr = &mut BitReader::new(hdrs);
    let mut recipe = header::Recipe::default();
    let mut blocks = Vec::with_capacity(layout.blocks.len());
    let mut starts = Vec::with_capacity(layout.blocks.len() + 1);
    let mut k = 0;
    let nb = layout.blocks.len();
    for (i, (b, &c)) in layout.blocks.into_iter().zip(&counts).enumerate() {
        let t = &tokens[k..k + c];
        starts.push(k);
        k += c;
        let body = match b {
            Shape::Stored { pad, len } => Body::Stored { pad, len },
            Shape::Coded { btype: 1, .. } => Body::Fixed { tokens: vec![] },
            Shape::Coded { .. } => Body::Dynamic {
                header: header::read(hr, t, &mut recipe)?,
                tokens: vec![],
            },
        };
        blocks.push(Block {
            last: i + 1 == nb,
            body,
        });
    }
    starts.push(k);
    let stream = Stream {
        blocks,
        tail_pad: layout.tail_pad,
        trailing: layout.trailing,
    };
    write::write(&stream, &starts, &tokens, data).map_err(|_| Error::Corrupt)
}

/// `f(0..n)` on up to `threads` threads, in order.
fn par_map<R: Send>(n: usize, threads: usize, f: impl Fn(usize) -> R + Sync) -> Vec<R> {
    if threads <= 1 || n <= 1 {
        return (0..n).map(f).collect();
    }
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    let next = AtomicUsize::new(0);
    let out: Vec<Mutex<Option<R>>> = (0..n).map(|_| Mutex::new(None)).collect();
    std::thread::scope(|s| {
        for _ in 0..threads.min(n) {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    if i >= n {
                        break;
                    }
                    *out[i].lock().unwrap() = Some(f(i));
                }
            });
        }
    });
    out.into_iter()
        .map(|m| m.into_inner().unwrap().expect("every item runs"))
        .collect()
}
