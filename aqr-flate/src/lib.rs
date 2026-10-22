//! Deconstructs raw DEFLATE streams into their decompressed data and a record of corrections,
//! and reconstructs the bit-exact original from the two: tokens are predicted from the
//! data by a rule (and, in the adaptive profiles, by zlib emulation), and only the
//! mispredictions are stored.
//!
//! ```no_run
//! # let raw: Vec<u8> = vec![];
//! let (data, meta) = aqr_flate::deconstruct(&raw)?;
//! assert_eq!(aqr_flate::reconstruct(&data, &meta)?, raw);
//! # Ok::<(), aqr_flate::Error>(())
//! ```
//!
//! The prediction at a position depends on nothing but the data, so the tokens are cut into
//! segments that are coded and rebuilt in parallel. With static codes (profiles 0-9),
//! rebuilding a correctly predicted token costs one match search and nothing else: the
//! corrections are a sparse list read only at the misses.
//!
//! The metadata is a record: a profile (u16, little endian; see [`profiles`]) and a body of
//! the lengths of the two bit streams that follow (main and headers), then each segment's
//! corrections, byte aligned. FORMAT.md describes it in full.
//! - main: the rule; the blocks (type, token count or stored length); padding and trailing
//!   bytes; the coding's parameters (static: the correction codes; adaptive: zlib's
//!   parameters); per segment its token count, start and size (static: also its misses;
//!   adaptive: also the emulator's state).
//! - headers: each dynamic block's Huffman header, against zlib's trees for its tokens.
//! - corrections: static codes (profiles 0-9), or adaptive binary arithmetic coding with
//!   lookahead and zlib emulation ([`adapt`], profiles from [`ADAPTIVE`] on).

pub mod adapt;
mod arith;
pub mod bits;
pub mod code;
pub mod deflate;
pub mod detect;
pub mod formula;
pub mod header;
pub mod huffman;
pub mod inflate;
pub mod matchfind;
#[doc(hidden)]
pub mod table;
pub mod tokens;
#[doc(hidden)]
pub mod trees;
pub mod write;
pub mod zlib;

use std::fmt;

pub use detect::Hints;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The DEFLATE stream ended prematurely.
    Truncated,
    /// The DEFLATE stream is not valid.
    Invalid(&'static str),
    /// The metadata does not describe a valid stream for the given data.
    Corrupt,
    /// Deconstruction did not reproduce its input; a bug.
    Inconsistent,
    /// Bad parameters or priors.
    Param(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Truncated => f.write_str("truncated DEFLATE stream"),
            Error::Invalid(m) => write!(f, "invalid DEFLATE stream: {m}"),
            Error::Corrupt => f.write_str("corrupt metadata"),
            Error::Inconsistent => f.write_str("internal error: deconstruction is inconsistent"),
            Error::Param(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

/// The number of threads reconstruction uses by default: the available parallelism.
pub fn default_threads() -> usize {
    std::thread::available_parallelism().map_or(1, |n| n.get())
}

use crate::bits::{BitReader, BitWriter};
use crate::deflate::{Block, Body, Stream, Token};
use crate::zlib::{Checkpoint, Emulator};
use adapt::{Backend, Priors};
use code::{get_usize, get_varint, put_gamma, put_varint};
pub use tokens::Rule;
use tokens::{FIELDS, Range};

/// The speed penalty λ of the static profiles' rule catalogs (bits per kilobyte of data for
/// every ns/B a rule takes; see `aqr-flate-train rules`): profile `k < 10` uses
/// `LAMBDAS[k]`'s, from the fastest to the smallest.
pub const LAMBDAS: [u16; 10] = [200, 100, 50, 30, 20, 10, 5, 2, 1, 0];
const CATALOGS: [&str; 10] = [
    include_str!("../model/rules-l200.txt"),
    include_str!("../model/rules-l100.txt"),
    include_str!("../model/rules-l50.txt"),
    include_str!("../model/rules-l30.txt"),
    include_str!("../model/rules-l20.txt"),
    include_str!("../model/rules-l10.txt"),
    include_str!("../model/rules-l5.txt"),
    include_str!("../model/rules-l2.txt"),
    include_str!("../model/rules-l1.txt"),
    include_str!("../model/rules-l0.txt"),
];
/// The first adaptive profile: profile `ADAPTIVE + k` codes with the `k`th set of priors, and
/// the λ = 0 catalog.
pub const ADAPTIVE: u16 = 10;
pub(crate) const PRIORS: [&str; 1] = [include_str!("../model/priors-10.txt")];
/// The profile of [`Options::default`] (λ = 5).
pub const DEFAULT_PROFILE: u16 = 6;

/// The known profiles.
pub fn profiles() -> std::ops::Range<u16> {
    0..ADAPTIVE + PRIORS.len() as u16
}

/// The rule catalog a profile chooses from and indexes rules in, with its fingerprint map.
pub fn profile_catalog(profile: u16) -> Result<&'static (Vec<Rule>, Map)> {
    static C: [std::sync::OnceLock<(Vec<Rule>, Map)>; 10] =
        [const { std::sync::OnceLock::new() }; 10];
    let k = if profile >= ADAPTIVE {
        CATALOGS.len() - 1
    } else {
        profile as usize
    };
    if !profiles().contains(&profile) {
        return Err(Error::Param(format!("unknown profile {profile}")));
    }
    Ok(C[k].get_or_init(|| parse_catalog(CATALOGS[k]).expect("built-in rules are valid")))
}

/// What a record is read with besides the data: the rules of its profile's catalog, and the
/// default models (static profiles) or the priors (adaptive ones). The profiles' are built in
/// ([`profile_data`]); the formula module takes them as resources.
#[derive(Clone, Copy)]
pub struct ModelData<'a> {
    pub rules: &'a [Rule],
    pub coding: Coding<'a>,
}

#[derive(Clone, Copy)]
pub enum Coding<'a> {
    Static(&'a [code::Model; FIELDS]),
    Adaptive(&'a Priors),
}

/// The built-in model data of a profile.
pub fn profile_data(profile: u16) -> Result<ModelData<'static>> {
    let rules = &profile_catalog(profile)?.0;
    let coding = if profile >= ADAPTIVE {
        Coding::Adaptive(profile_priors(profile)?)
    } else {
        Coding::Static(default_models())
    };
    Ok(ModelData { rules, coding })
}

/// The profile of a record.
pub fn record_profile(meta: &[u8]) -> Result<u16> {
    Ok(u16::from_le_bytes(
        *meta.first_chunk::<2>().ok_or(Error::Corrupt)?,
    ))
}

/// The priors of an adaptive profile.
pub fn profile_priors(profile: u16) -> Result<&'static Priors> {
    static P: [std::sync::OnceLock<Priors>; PRIORS.len()] =
        [const { std::sync::OnceLock::new() }; PRIORS.len()];
    let k = profile
        .checked_sub(ADAPTIVE)
        .map(usize::from)
        .filter(|&k| k < PRIORS.len())
        .ok_or_else(|| Error::Param(format!("profile {profile} is not adaptive")))?;
    Ok(P[k].get_or_init(|| adapt::parse_priors(PRIORS[k]).expect("built-in priors are valid")))
}

#[derive(Debug, Clone)]
pub struct Options {
    /// The profile written: the catalog rules are indexed in, and how corrections are coded.
    pub profile: u16,
    /// With an adaptive profile, the priors to code with instead of the profile's (for
    /// training them; the metadata is then only readable with the same priors).
    pub priors: Option<Priors>,
    /// With an adaptive profile, whether to try zlib emulation.
    pub zlib: bool,
    /// With an adaptive profile, whether to try a chain rule ([`tokens::Kind::Chain`]).
    pub chain: bool,
    /// With an adaptive profile, collect the first uses of each context into
    /// [`Report::seen`] (for training priors).
    pub collect: bool,
    /// Rules to choose from.
    pub rules: Vec<Rule>,
    /// The rule (index in `rules`) for streams by their fingerprint; those
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

impl Options {
    /// The defaults for `profile`, with its catalog.
    pub fn for_profile(profile: u16) -> Result<Options> {
        let (rules, map) = profile_catalog(profile)?.clone();
        Ok(Options {
            profile,
            rules,
            map,
            ..Default::default()
        })
    }
}

impl Default for Options {
    fn default() -> Self {
        let (rules, map) = profile_catalog(DEFAULT_PROFILE).unwrap().clone();
        Options {
            profile: DEFAULT_PROFILE,
            priors: None,
            zlib: true,
            chain: true,
            collect: false,
            rules,
            map,
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

/// The built-in default models of the correction fields (`model/models.txt`): one
/// line per field, the rounded frequencies of its symbols.
pub fn default_models() -> &'static [code::Model; FIELDS] {
    static M: std::sync::OnceLock<[code::Model; FIELDS]> = std::sync::OnceLock::new();
    M.get_or_init(|| {
        let mut lines = include_str!("../model/models.txt")
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

/// The rule catalog of the default profile.
pub fn catalog() -> Vec<Rule> {
    profile_catalog(DEFAULT_PROFILE).unwrap().0.clone()
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
    /// The parsed stream with its blocks' tokens in place.
    fn stream_with_tokens(&self) -> Stream {
        let mut stream = self.stream.clone();
        for (b, s) in stream.blocks.iter_mut().zip(self.starts.windows(2)) {
            if let Body::Fixed { tokens } | Body::Dynamic { tokens, .. } = &mut b.body {
                *tokens = self.tokens[s[0]..s[1]].to_vec();
            }
        }
        stream
    }

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

pub fn deconstruct_with(raw: &[u8], opts: &Options) -> Result<(Vec<u8>, Vec<u8>)> {
    deconstruct_report(raw, opts).map(|(d, m, _)| (d, m))
}

/// What the metadata of a stream consists of.
#[derive(Debug, Clone, Default)]
pub struct Report {
    pub rule: Rule,
    pub tokens: usize,
    pub misses: usize,
    /// Bytes of the block layout, coding parameters and segment directory (the rule
    /// included).
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
    /// zlib's parameters, if the corrections are coded with zlib emulation.
    pub zlib: Option<zlib::Params>,
    /// With [`Options::collect`]: per context of [`adapt`], its uses among the first
    /// [`adapt::FIRST`] of every segment and the ones among them.
    pub seen: Vec<[u32; 2]>,
    /// Seconds spent parsing, choosing the rule, coding the tokens, coding the headers, and
    /// on the rest.
    pub times: [f64; 5],
}

/// Like [`deconstruct_with`], also describing the metadata.
pub fn deconstruct_report(raw: &[u8], opts: &Options) -> Result<(Vec<u8>, Vec<u8>, Report)> {
    deconstruct_hinted(raw, opts, &Hints::default())
}

/// Like [`deconstruct_report`], with what the stream's container says about its encoder
/// (e.g. [`Hints::zlib_header`]): zlib emulation only considers parameters `hints` allows.
/// The metadata does not depend on hints to be read.
pub fn deconstruct_hinted(
    raw: &[u8],
    opts: &Options,
    hints: &Hints,
) -> Result<(Vec<u8>, Vec<u8>, Report)> {
    let threads = opts.threads.max(1);
    let profile = opts.profile;
    let catalog = &profile_catalog(profile)?.0;
    let adaptive = profile >= ADAPTIVE;
    let priors = match &opts.priors {
        Some(p) if adaptive => Some(p),
        _ if adaptive => Some(profile_priors(profile)?),
        _ => None,
    };
    let t0 = std::time::Instant::now();
    let f = Flat::new(raw, opts.segment, threads, opts.verify)?;
    // What a record cannot hold, or reconstruct would reject (FORMAT.md, "Domain").
    let n = f.data.len();
    if n >= u32::MAX as usize
        || f.stream.trailing.len() > 1 << 30
        || f.stream.blocks.len() > n + (1 << 20)
    {
        return Err(Error::Invalid("beyond the limits of a record"));
    }
    let t1 = std::time::Instant::now();

    let t4 = std::time::Instant::now();
    let (headers, first_recipe) = headers(&f);
    let fp = fingerprint(&f, first_recipe.unwrap_or(u32::MAX));
    let ranges = segment_ranges(&f);
    let t5 = std::time::Instant::now();
    let rule = choose_rule(&f, opts, fp)?;
    let t2 = std::time::Instant::now();
    let choice = match priors {
        Some(priors) => Some(choose_adaptive(
            &f,
            rule,
            opts,
            priors,
            hints,
            segment_ranges(&f),
            threads,
        )?),
        None => None,
    };
    let rule = choice.as_ref().map_or(rule, |c| c.rule);

    let mut w = BitWriter::new();
    // The rule as its index in the profile's catalog, or in full.
    match catalog.iter().position(|r| *r == rule) {
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
    let mut report = Report {
        rule,
        tokens: f.tokens.len(),
        headers: headers.len(),
        counts: vec![vec![0; code::ALPHABET]; FIELDS],
        fingerprint: fp,
        ..Default::default()
    };
    let bodies = if let Some(AdaptiveChoice {
        zlib, segs, ranges, ..
    }) = choice
    {
        report.zlib = zlib;
        report.misses = segs.iter().map(|s| s.misses).sum();
        if opts.collect {
            report.seen = vec![[0; 2]; adapt::NUM];
            for s in &segs {
                for (a, b) in report.seen.iter_mut().zip(&s.seen) {
                    a[0] += b[0];
                    a[1] += b[1];
                }
            }
        }
        match zlib {
            None => w.bits(1, 0),
            Some(p) => {
                w.bits(1, 1);
                w.bits(4, p.level as u32);
                w.bits(3, p.strategy as u32);
                w.bits(3, p.wbits as u32 - 9);
                w.bits(4, p.mem as u32);
            }
        }
        put_gamma(&mut w, ranges.len() as u64);
        for (i, (r, s)) in ranges.iter().zip(&segs).enumerate() {
            put_gamma(&mut w, r.count as u64);
            if i > 0 {
                put_gamma(&mut w, r.pos as u64);
            }
            put_gamma(&mut w, s.body.len() as u64);
            if let (Some(p), true) = (zlib, i > 0) {
                match &s.cp {
                    None => w.bits(1, 0),
                    Some(cp) => {
                        w.bits(1, 1);
                        put_checkpoint(&mut w, cp, p.wbits);
                    }
                }
            }
        }
        segs.into_iter().map(|s| s.body).collect::<Vec<_>>()
    } else {
        let segs = par_map(ranges.len(), threads, |i| -> Result<tokens::Recorder> {
            let mut rec = tokens::Recorder::default();
            let r = &ranges[i];
            let actual = &f.tokens[r.first..r.first + r.count];
            tokens::code_range(&mut rec, &f.data, rule, r, Some(actual), None)?;
            Ok(rec)
        })
        .into_iter()
        .collect::<Result<Vec<_>>>()?;
        let mut counts: [code::Counts; FIELDS] = Default::default();
        for s in &segs {
            for (a, b) in counts.iter_mut().zip(&s.counts) {
                a.merge(b);
            }
        }
        let defaults = default_models();
        let before = w_bits(&w);
        let models: [code::Model; FIELDS] =
            std::array::from_fn(|k| code::Model::choose(&counts[k], &defaults[k], &mut w));
        report.tables = (w_bits(&w) - before).div_ceil(8);
        let bodies = par_map(segs.len(), threads, |i| segs[i].write(&models));
        put_gamma(&mut w, segs.len() as u64);
        for (i, ((s, r), body)) in segs.iter().zip(&ranges).zip(&bodies).enumerate() {
            put_gamma(&mut w, r.count as u64);
            put_gamma(&mut w, s.misses[0] as u64);
            put_gamma(&mut w, s.misses[1] as u64);
            if i > 0 {
                put_gamma(&mut w, r.pos as u64);
            }
            put_gamma(&mut w, body.len() as u64);
        }
        report.misses = segs.iter().map(|s| s.misses[0] + s.misses[1]).sum();
        report.fields = std::array::from_fn(|k| counts[k].cost() / 8.0);
        report.coded = std::array::from_fn(|k| models[k].bits(&counts[k]) / 8.0);
        report.counts = counts.iter().map(|c| c.0.to_vec()).collect();
        bodies
    };
    let t3 = std::time::Instant::now();
    w.bits(w.pad_len(), 0);
    let main = w.finish();
    report.main = main.len();
    report.corrections = bodies.iter().map(Vec::len).sum();

    let secs = |a: std::time::Instant, b: std::time::Instant| (b - a).as_secs_f64();
    report.times = [
        secs(t0, t1),
        secs(t5, t2),
        secs(t2, t3),
        secs(t4, t5),
        secs(t3, std::time::Instant::now()),
    ];
    let mut meta = profile.to_le_bytes().to_vec();
    put_varint(&mut meta, main.len() as u64);
    put_varint(&mut meta, headers.len() as u64);
    meta.extend(main);
    meta.extend(headers);
    for b in bodies {
        meta.extend(b);
    }
    Ok((f.data, meta, report))
}

/// The segments' token ranges (the tokens are cut where the parser cut them).
fn segment_ranges(f: &Flat) -> Vec<Range<'_>> {
    let (cuts, cut_pos) = (&f.stats.cuts, &f.stats.cut_pos);
    let n = cuts.len().saturating_sub(1);
    (0..n)
        .map(|i| Range {
            first: cuts[i],
            count: cuts[i + 1] - cuts[i],
            pos: cut_pos[i],
            end: if i + 1 < n {
                cut_pos[i + 1]
            } else {
                f.data.len()
            },
            skips: &f.skips,
        })
        .collect()
}

/// How an adaptive profile codes a stream: the rule, zlib emulation, and the segments.
struct AdaptiveChoice<'a> {
    rule: Rule,
    zlib: Option<zlib::Params>,
    segs: Vec<AdaptiveSeg>,
    ranges: Vec<Range<'a>>,
}

/// The smallest adaptive coding among: the catalog's `rule`; a chain rule
/// ([`chain_rule`]); and, for a stream that looks like zlib's, the better of the two with
/// zlib emulation (with parameters `hints` allows). Hinted parameters that predict every
/// token are taken with `rule` alone.
fn choose_adaptive<'a>(
    f: &'a Flat,
    rule: Rule,
    opts: &Options,
    priors: &Priors,
    hints: &Hints,
    ranges: Vec<Range<'a>>,
    threads: usize,
) -> Result<AdaptiveChoice<'a>> {
    let collect = opts.collect;
    let size = |v: &[AdaptiveSeg]| v.iter().map(|s| s.body.len() + 4).sum::<usize>();
    let stream = opts.zlib.then(|| f.stream_with_tokens());
    // Hinted parameters that predict every token leave nothing to choose.
    if let (Some(stream), Some(p)) = (&stream, hints.params)
        && crate::detect::plausible(stream, &f.data, p, hints)
    {
        let zr = zlib_ranges(f, p, &ranges);
        let segs = code_adaptive(f, rule, Some(p), priors, &zr, threads, collect)?;
        if segs.iter().all(|s| s.misses == 0) {
            // In a short stream the rule may predict as well, without zlib's parameters (ties
            // go to the rule, as below).
            if f.data.len() < HINT_RULE_BELOW {
                let alone = code_adaptive(f, rule, None, priors, &ranges, threads, collect)?;
                if size(&alone) <= size(&segs) {
                    return Ok(AdaptiveChoice {
                        rule,
                        zlib: None,
                        segs: alone,
                        ranges,
                    });
                }
            }
            return Ok(AdaptiveChoice {
                rule,
                zlib: Some(p),
                segs,
                ranges: zr,
            });
        }
    }
    let mut best = AdaptiveChoice {
        rule,
        zlib: None,
        segs: code_adaptive(f, rule, None, priors, &ranges, threads, collect)?,
        ranges,
    };
    if opts.chain
        && let Some(chain) = chain_rule(f, priors)?
    {
        let segs = code_adaptive(f, chain, None, priors, &best.ranges, threads, collect)?;
        if size(&segs) < size(&best.segs) {
            (best.rule, best.segs) = (chain, segs);
        }
    }
    let Some(p) = stream.and_then(|s| zlib_params(&s, &f.data, hints)) else {
        return Ok(best);
    };
    let ranges = zlib_ranges(f, p, &best.ranges);
    let segs = code_adaptive(f, best.rule, Some(p), priors, &ranges, threads, collect)?;
    if size(&segs) < size(&best.segs) {
        best = AdaptiveChoice {
            rule: best.rule,
            zlib: Some(p),
            segs,
            ranges,
        };
    }
    Ok(best)
}

/// Below this many bytes of data, the catalog's rule alone is compared with exact hinted
/// zlib parameters (it is cheap there, and often smaller).
const HINT_RULE_BELOW: usize = 1 << 10;

/// Chain configurations tried, as (depth, nice) indices into [`tokens::DEPTHS`] and
/// [`tokens::NICES`]: depth 128 and nice 128, 1 and 8, 16 and 16, 4096 and 128.
const CHAIN_CONFIGS: [(u8, u8); 4] = [(7, 5), (0, 0), (4, 1), (12, 5)];
/// No chain searches deeper than this quantile of the depths at which the stream's matches
/// are (the rest are coded by rank anyway): joining stays fast, and the intermediate depths
/// this gives often predict better too.
const DEPTH_QUANTILE: f64 = 0.995;
/// Chain configurations are compared on this much data when the stream has twice as much.
const CHAIN_PREFIX: usize = 256 << 10;

/// The best chain rule for a stream, by adaptive cost: of [`CHAIN_CONFIGS`] with the depth
/// capped by [`DEPTH_QUANTILE`], compared on a prefix of long streams. `None` without
/// matches.
fn chain_rule(f: &Flat, priors: &Priors) -> Result<Option<Rule>> {
    let mut matches = vec![];
    let mut p = 0;
    let mut skips = f.skips.iter().peekable();
    for (k, t) in f.tokens.iter().enumerate() {
        while let Some(&&(at, len)) = skips.peek()
            && at == k
        {
            p += len;
            skips.next();
        }
        if let Token::Match { dist, .. } = *t {
            matches.push((p, dist as usize));
        }
        p += t.span();
    }
    if matches.is_empty() {
        return Ok(None);
    }
    let dq = crate::matchfind::Matcher::single(&f.data).depth_quantile(
        &f.data,
        matches.into_iter(),
        DEPTH_QUANTILE,
    );
    let cap = tokens::DEPTHS
        .iter()
        .position(|&d| d as usize > dq)
        .unwrap_or(tokens::DEPTHS.len() - 1) as u8;
    let mut rules: Vec<Rule> = CHAIN_CONFIGS
        .iter()
        .map(|&(depth, nice)| Rule {
            kind: tokens::Kind::Chain,
            depth: depth.min(cap),
            nice,
            ..Rule::default()
        })
        .collect();
    rules.sort();
    rules.dedup();
    if rules.len() == 1 {
        return Ok(Some(rules[0]));
    }
    let prefix = if f.data.len() >= 2 * CHAIN_PREFIX {
        CHAIN_PREFIX
    } else {
        usize::MAX
    };
    let p = Prepared::from_flat(f, prefix);
    let mut best = (f64::INFINITY, rules[0]);
    for r in rules {
        let mut a = adapt::Adaptive::new(adapt::Cost::default(), priors, None);
        p.code(r, &mut a)?;
        if a.b.0 < best.0 {
            best = (a.b.0, r);
        }
    }
    Ok(Some(best.1))
}

/// zlib's parameters among those `hints` allows, if the stream looks like zlib's (see
/// [`crate::detect`]).
fn zlib_params(stream: &Stream, data: &[u8], hints: &Hints) -> Option<zlib::Params> {
    let candidates = crate::detect::candidates_hinted(stream, hints);
    if candidates.is_empty() {
        return None;
    }
    crate::detect::detect_refined(stream, data, candidates).map(|d| d.params)
}

/// The segments to code with zlib emulation under `p`: `ranges`, or the whole stream as one
/// if the emulation cannot resume at a segment's start (it would only serve the first one).
fn zlib_ranges<'a>(f: &'a Flat, p: zlib::Params, ranges: &[Range<'a>]) -> Vec<Range<'a>> {
    if p.resumable() || ranges.len() <= 1 {
        ranges.iter().map(|r| Range { ..*r }).collect()
    } else {
        vec![Range {
            first: 0,
            count: f.tokens.len(),
            pos: 0,
            end: f.data.len(),
            skips: &f.skips,
        }]
    }
}

/// A segment's adaptively coded corrections.
struct AdaptiveSeg {
    body: Vec<u8>,
    /// The emulator's state it starts from (not for the first segment, which starts it).
    cp: Option<Checkpoint>,
    misses: usize,
    seen: Vec<[u32; 2]>,
}

/// Codes the segments adaptively, with zlib emulation under `zlib`. Each segment but the first
/// resumes the emulation from its state where the previous one ended, if it can be resumed
/// there (and was not lost on the way); segments are then coded one after another.
fn code_adaptive<'a>(
    f: &'a Flat,
    rule: Rule,
    zlib: Option<zlib::Params>,
    priors: &Priors,
    ranges: &[Range],
    threads: usize,
    collect: bool,
) -> Result<Vec<AdaptiveSeg>> {
    let code = |i: usize, cp: Option<Checkpoint>, emu: Option<Emulator<'a>>| {
        let r = &ranges[i];
        let mut a = adapt::Adaptive::new(adapt::Enc::new(), priors, emu);
        if collect {
            a.seen = Some(vec![[0; 2]; adapt::NUM]);
        }
        let actual = &f.tokens[r.first..r.first + r.count];
        tokens::code_range(&mut a, &f.data, rule, r, Some(actual), None)?;
        let emu = a.take_emulator();
        let seg = AdaptiveSeg {
            cp,
            misses: a.misses,
            seen: a.seen.take().unwrap_or_default(),
            body: a.b.finish(),
        };
        Ok::<_, Error>((seg, emu))
    };
    let Some(p) = zlib else {
        return par_map(ranges.len(), threads, |i| code(i, None, None).map(|r| r.0))
            .into_iter()
            .collect();
    };
    let mut emu = Some(Emulator::new(&f.data, p));
    let mut out = Vec::with_capacity(ranges.len());
    for (i, r) in ranges.iter().enumerate() {
        let mut cp = None;
        if i > 0 {
            cp = emu
                .as_ref()
                .and_then(Emulator::checkpoint)
                .filter(|cp| cp.strstart - cp.match_available as usize == r.pos);
            // Both ways the segment is coded with the emulator rebuilt from the checkpoint.
            emu = cp.as_ref().and_then(|cp| Emulator::resume(&f.data, p, cp));
            if emu.is_none() {
                cp = None;
            }
        }
        let (seg, e) = code(i, cp, emu)?;
        emu = e;
        out.push(seg);
    }
    Ok(out)
}

/// An emulator's state at the start of a segment, relative to where it normally is.
fn put_checkpoint(w: &mut BitWriter, cp: &Checkpoint, wbits: u8) {
    let ws = 1u64 << wbits;
    let (s, b) = (cp.strstart as u64, cp.base as u64);
    w.bits(1, cp.match_available as u32);
    put_gamma(w, s / ws - b / ws);
    put_gamma(w, b + 2 * ws - cp.read as u64);
    put_gamma(w, 2 * ws - cp.high_water as u64);
    put_gamma(w, cp.match_length as u64 - 2);
    if cp.match_length > 2 {
        put_gamma(w, s - cp.match_start as u64);
    }
}

/// Inverse of [`put_checkpoint`] for a segment starting at `pos` in `n` bytes of data.
fn get_checkpoint(r: &mut BitReader, pos: usize, wbits: u8, n: usize) -> Result<Checkpoint> {
    let ws = 1usize << wbits;
    let match_available = r.bits(1)? == 1;
    let strstart = pos + match_available as usize;
    let base = (strstart / ws)
        .checked_sub(get_usize(r, n)?)
        .ok_or(Error::Corrupt)?
        * ws;
    let read = (base + 2 * ws)
        .checked_sub(get_usize(r, 2 * ws)?)
        .ok_or(Error::Corrupt)?;
    let high_water = (2 * ws)
        .checked_sub(get_usize(r, 2 * ws)?)
        .ok_or(Error::Corrupt)?;
    let match_length = 2 + get_usize(r, crate::matchfind::MAX_MATCH)?;
    let match_start = if match_length > 2 {
        strstart
            .checked_sub(get_usize(r, ws)?)
            .ok_or(Error::Corrupt)?
    } else {
        0
    };
    Ok(Checkpoint {
        strstart,
        base,
        read,
        high_water,
        match_available,
        match_length,
        match_start,
    })
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
                    Kind::Chain => false,
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

    /// Codes the tokens under `rule` into `c`, as one range.
    pub fn code<C: tokens::Corr>(&self, rule: Rule, c: &mut C) -> Result<()> {
        let range = Range {
            first: 0,
            count: self.tokens.len(),
            pos: 0,
            end: self.data.len(),
            skips: &self.skips,
        };
        tokens::code_range(c, &self.data, rule, &range, Some(&self.tokens), None)?;
        Ok(())
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

/// A stream's fingerprint and the estimated cost in bits of each rule over the first
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
    reconstruct_with(data, meta, profile_data(record_profile(meta)?)?, threads)
}

/// [`reconstruct_with_threads`] with the model data `model` in place of the profile's built-in
/// one. The profile still tells static coding from adaptive, which `model` must agree with.
pub fn reconstruct_with(
    data: &[u8],
    meta: &[u8],
    model: ModelData,
    threads: usize,
) -> Result<Vec<u8>> {
    let threads = threads.max(1);
    let n = data.len();
    let profile = record_profile(meta)?;
    if !profiles().contains(&profile) {
        return Err(Error::Param(format!("unknown profile {profile}")));
    }
    let catalog = model.rules;
    let adaptive = profile >= ADAPTIVE;
    let (defaults, priors) = match model.coding {
        Coding::Static(m) if !adaptive => (Some(m), None),
        Coding::Adaptive(p) if adaptive => (None, Some(p)),
        _ => return Err(Error::Corrupt),
    };
    let mut at = 2;
    let main_len = get_varint(meta, &mut at)? as usize;
    let hdr_len = get_varint(meta, &mut at)? as usize;
    let main = meta
        .get(at..at.checked_add(main_len).ok_or(Error::Corrupt)?)
        .ok_or(Error::Corrupt)?;
    at += main_len;
    let hdrs = meta
        .get(at..at.checked_add(hdr_len).ok_or(Error::Corrupt)?)
        .ok_or(Error::Corrupt)?;
    at += hdr_len;

    let r = &mut BitReader::new(main);
    let rule = if r.bits(1)? == 1 {
        *catalog.get(get_usize(r, 1 << 16)?).ok_or(Error::Corrupt)?
    } else {
        Rule::read(r)?
    };
    let layout = read_blocks(r, n)?;
    // The coding's parameters.
    let mut models = None;
    let mut zlib = None;
    if adaptive {
        if r.bits(1)? == 1 {
            let p = zlib::Params {
                level: r.bits(4)? as u8,
                strategy: *zlib::Strategy::ALL
                    .get(r.bits(3)? as usize)
                    .ok_or(Error::Corrupt)?,
                wbits: r.bits(3)? as u8 + 9,
                mem: r.bits(4)? as u8,
            };
            if !(1..=9).contains(&p.level) || !(1..=9).contains(&p.mem) || p.wbits > 15 {
                return Err(Error::Corrupt);
            }
            zlib = Some(p);
        }
    } else {
        let mut m = Vec::with_capacity(FIELDS);
        for d in defaults.unwrap() {
            m.push(code::Model::read(r, d)?);
        }
        let m: [code::Model; FIELDS] = m.try_into().map_err(|_| Error::Corrupt)?;
        models = Some(m);
    }
    let nseg = get_usize(r, n + 1)?;
    // (first token, count, misses, position, body, emulator state)
    let mut segs = Vec::with_capacity(nseg);
    let mut first = 0;
    for i in 0..nseg {
        let count = get_usize(r, n)?;
        let misses = if adaptive {
            [0, 0]
        } else {
            [get_usize(r, count)?, get_usize(r, count)?]
        };
        let pos = if i > 0 { get_usize(r, n)? } else { 0 };
        let len = get_usize(r, meta.len())?;
        let cp = match zlib {
            Some(p) if i > 0 && r.bits(1)? == 1 => Some(get_checkpoint(r, pos, p.wbits, n)?),
            _ => None,
        };
        let body = meta.get(at..at + len).ok_or(Error::Corrupt)?;
        at += len;
        segs.push((first, count, misses, pos, body, cp));
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
        let (first, count, misses, pos, body, cp) = segs[i];
        let end = segs.get(i + 1).map_or(n, |s| s.3);
        let range = Range {
            first,
            count,
            pos,
            end,
            skips: &skips,
        };
        let mut out = Vec::with_capacity(count);
        let p = match (&models, priors) {
            (Some(models), _) => {
                let mut player = tokens::Player::new(body, models, misses)?;
                let p = tokens::code_range(&mut player, data, rule, &range, None, Some(&mut out))?;
                if !player.done() {
                    return Err(Error::Corrupt);
                }
                p
            }
            (None, Some(priors)) => {
                let emu = match (zlib, cp) {
                    (Some(p), None) if i == 0 => Some(Emulator::new(data, p)),
                    (Some(p), Some(cp)) => {
                        Some(Emulator::resume(data, p, &cp).ok_or(Error::Corrupt)?)
                    }
                    _ => None,
                };
                let mut a = adapt::Adaptive::new(adapt::Dec::new(body), priors, emu);
                let p = tokens::code_range(&mut a, data, rule, &range, None, Some(&mut out))?;
                if a.b.failed() {
                    return Err(Error::Corrupt);
                }
                p
            }
            _ => unreachable!(),
        };
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
