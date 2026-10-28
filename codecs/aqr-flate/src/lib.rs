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

// The code of the modules alone uses only part of the rest.
#![cfg_attr(not(feature = "encode"), allow(unused))]
pub mod adapt;
mod arith;
pub mod bits;
pub mod code;
pub mod deflate;
pub mod detect;
#[cfg(feature = "encode")]
pub mod formula;
pub mod header;
pub mod huffman;
pub mod inflate;
pub mod matchfind;
pub mod module;
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
    /// The stream has more data than allowed ([`Options::max_data`]).
    Limit,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Truncated => f.write_str("truncated DEFLATE stream"),
            Error::Invalid(m) => write!(f, "invalid DEFLATE stream: {m}"),
            Error::Corrupt => f.write_str("corrupt metadata"),
            Error::Inconsistent => f.write_str("internal error: deconstruction is inconsistent"),
            Error::Param(m) => write!(f, "{m}"),
            Error::Limit => f.write_str("DEFLATE stream of more data than allowed"),
        }
    }
}

impl std::error::Error for Error {}

/// As the error of a task (reconstruction on WebAssembly): `Invalid`'s message is not kept.
impl aqr_common::wire::Wire for Error {
    fn put(&self, out: &mut Vec<u8>) {
        match self {
            Error::Truncated => out.push(0),
            Error::Invalid(_) => out.push(1),
            Error::Corrupt => out.push(2),
            Error::Inconsistent => out.push(3),
            Error::Param(m) => {
                out.push(4);
                m.put(out);
            }
            Error::Limit => out.push(5),
        }
    }

    fn get(b: &mut &[u8]) -> Option<Self> {
        Some(match u8::get(b)? {
            0 => Error::Truncated,
            1 => Error::Invalid("(in a task)"),
            2 => Error::Corrupt,
            3 => Error::Inconsistent,
            4 => Error::Param(String::get(b)?),
            5 => Error::Limit,
            _ => return None,
        })
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// The number of threads reconstruction uses by default: the available parallelism.
pub fn default_threads() -> usize {
    std::thread::available_parallelism().map_or(1, |n| n.get())
}

use crate::bits::{BitReader, BitWriter};
use crate::deflate::{Block, Body, Stream, Token};
use crate::zlib::{Checkpoint, Emulator, Followed};
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
    /// Streams of more data than this are refused ([`Error::Limit`]), before it is all made.
    pub max_data: usize,
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
            max_data: usize::MAX,
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

/// Hands the tokens of a stream parsed again to `send` by segment, as each is complete.
struct Split<'a, F: FnMut(usize, Vec<Token>)> {
    cuts: &'a [usize],
    seg: usize,
    n: usize,
    cur: Vec<Token>,
    send: F,
}

impl<F: FnMut(usize, Vec<Token>)> Split<'_, F> {
    #[inline(always)]
    fn push(&mut self, t: Token) {
        self.cur.push(t);
        self.n += 1;
        while self.cuts.get(self.seg + 1) == Some(&self.n) {
            let next = self.cuts.get(self.seg + 2).map_or(0, |&c| c - self.n);
            (self.send)(
                self.seg,
                std::mem::replace(&mut self.cur, Vec::with_capacity(next)),
            );
            self.seg += 1;
        }
    }
}

impl<F: FnMut(usize, Vec<Token>)> inflate::Sink for Split<'_, F> {
    fn lit(&mut self, b: u8) {
        self.push(Token::Lit(b));
    }

    fn mat(&mut self, len: u16, dist: u16, alt258: bool) {
        self.push(Token::Match { len, dist, alt258 });
    }

    fn stored(&mut self, _: &[u8]) {}
}

/// The segments of `f` (`ranges`) coded by `rule`, their tokens parsed again from `raw` and
/// given to `threads` threads as each segment is complete, then dropped: a few segments' at
/// a time rather than the whole stream's. With one thread or one segment, each is coded as it
/// is parsed, on this thread.
fn code_streamed(
    raw: &[u8],
    f: &Flat,
    rule: Rule,
    ranges: &[Range],
    threads: usize,
) -> Result<Vec<tokens::Recorder>> {
    use std::sync::{Mutex, mpsc};
    let code = |i: usize, toks: &[Token]| {
        let mut rec = tokens::Recorder::default();
        let mut t = Ticking::new(&mut rec, f, &ranges[i], (0, &ONCE));
        tokens::code_range(&mut t, &f.data, rule, &ranges[i], Some(toks), None)?;
        t.end();
        Ok(rec)
    };
    let serial = || {
        let mut done = Vec::with_capacity(ranges.len());
        let mut failed = None;
        split_parsed(raw, &f.stats.cuts, |i, toks| match code(i, &toks) {
            Ok(rec) => done.push(rec),
            Err(e) => {
                failed.get_or_insert(e);
            }
        })?;
        match failed {
            Some(e) => Err(e),
            None if done.len() == ranges.len() => Ok(done),
            None => Err(Error::Inconsistent),
        }
    };
    let threads = threads.min(ranges.len());
    if threads <= 1 {
        return serial();
    }
    let done: Vec<Mutex<Option<Result<tokens::Recorder>>>> =
        ranges.iter().map(|_| Mutex::new(None)).collect();
    // at most two segments waiting per thread: the parse waits for the coding
    let (tx, rx) = mpsc::sync_channel::<(usize, Vec<Token>)>(2 * threads);
    let rx = Mutex::new(rx);
    // the parse here, the coding on the workers of the pool there are (or here too)
    let parsed = aqr_common::pool::scope(threads - 1, |scope| {
        if scope.workers() == 0 {
            return None;
        }
        for _ in 0..scope.workers() {
            scope.spawn(|| {
                loop {
                    // (the lock only while taking a segment, not while coding it)
                    let job = rx.lock().unwrap().recv();
                    let Ok((i, toks)) = job else { break };
                    *done[i].lock().unwrap() = Some(code(i, &toks));
                }
            });
        }
        let r = split_parsed(raw, &f.stats.cuts, |i, toks| {
            let _ = tx.send((i, toks));
        });
        drop(tx);
        Some(r)
    });
    match parsed {
        None => return serial(),
        Some(r) => r?,
    }
    done.into_iter()
        .map(|d| d.into_inner().unwrap().ok_or(Error::Inconsistent)?)
        .collect()
}

/// Parses `raw` again, giving `send` the tokens of each segment (as `cuts` mark them) in order
/// as it is complete, those at the end without tokens too.
fn split_parsed(raw: &[u8], cuts: &[usize], send: impl FnMut(usize, Vec<Token>)) -> Result<()> {
    let mut split = Split {
        cuts,
        seg: 0,
        n: 0,
        cur: Vec::new(),
        send,
    };
    let r = inflate::parse_head(raw, usize::MAX, &mut split);
    // segments without tokens at the end (none at all in a stream of stored blocks)
    while split.seg + 1 < split.cuts.len() {
        (split.send)(split.seg, std::mem::take(&mut split.cur));
        split.seg += 1;
    }
    r.map(|_| ())
}

/// Which pass of how many a coding of a stream is, for [`Flat::tell`]: the number may come
/// down as the coding goes.
type Pass<'a> = (usize, &'a std::sync::atomic::AtomicUsize);

/// The pass of a stream coded once.
static ONCE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(1);

impl Flat {
    /// Tells that `part` of the data is coded in pass `pass` ([`aqr_common::progress`]): as
    /// that share of the data, each pass of several taking its own.
    fn tell(&self, part: std::ops::Range<usize>, pass: Pass) {
        let (n, of) = (
            self.data.len(),
            pass.1.load(std::sync::atomic::Ordering::Relaxed).max(1),
        );
        let at = |p: usize| ((pass.0 * n + p.min(n)) / of).min(n);
        aqr_common::progress::done(&self.data[at(part.start)..at(part.end)]);
    }

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

    fn new(raw: &[u8], segment: usize, threads: usize, verify: bool, max: usize) -> Result<Flat> {
        let f = inflate::parse_segments_max(raw, segment, max)?;
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
    Parsed::new(raw, opts)?.deconstruct(opts, hints)
}

/// Bytes of data of the first run of tokens that [`Parsed::estimate`] codes (the others are
/// of [`inflate::CHUNK`] bytes, as [`inflate::next_chunk`] places them).
const ESTIMATE_FIRST: usize = 16 << 10;

/// A stream parsed, its data made, before anything is coded: so that whether to deconstruct
/// it can be decided by its data and [`Parsed::estimate`] without parsing it again.
pub struct Parsed<'a> {
    raw: &'a [u8],
    f: Flat,
    /// With static codes, the tokens are kept for a prefix only (to choose the rule and to
    /// estimate), and the stream is parsed again by segment (`code_streamed`).
    streamed: bool,
    secs: f64,
    chosen: std::cell::OnceCell<Chosen>,
}

/// What both [`Parsed::estimate`] and [`Parsed::deconstruct`] need.
struct Chosen {
    headers: Vec<u8>,
    fp: (u64, u64),
    rule: Rule,
    secs: [f64; 2],
}

impl<'a> Parsed<'a> {
    /// Parses `raw` for [`Parsed::deconstruct`] with `opts`, as [`deconstruct_with`] would.
    pub fn new(raw: &'a [u8], opts: &Options) -> Result<Parsed<'a>> {
        Parsed::parse(raw, opts, false)
    }

    /// [`Parsed::new`] for the stream at the start of `raw` alone: what follows it is not
    /// part of it (and not in the record), and [`Parsed::len`] tells where it ends.
    pub fn head(raw: &'a [u8], opts: &Options) -> Result<Parsed<'a>> {
        Parsed::parse(raw, opts, true)
    }

    fn parse(raw: &'a [u8], opts: &Options, head: bool) -> Result<Parsed<'a>> {
        let t0 = std::time::Instant::now();
        let streamed = opts.profile < ADAPTIVE && !opts.verify;
        let mut f = if streamed {
            let prefix = opts.prefix.max(ESTIMATE_FIRST);
            inflate::parse_segments_head(raw, opts.segment, prefix, opts.max_data)?
        } else {
            Flat::new(
                raw,
                opts.segment,
                opts.threads.max(1),
                opts.verify,
                opts.max_data,
            )?
        };
        let raw = if head {
            let end = raw.len() - f.stream.trailing.len();
            f.stream.trailing = Vec::new();
            &raw[..end]
        } else {
            raw
        };
        // What a record cannot hold, or reconstruct would reject (FORMAT.md, "Domain").
        let n = f.data.len();
        if n >= u32::MAX as usize
            || f.stream.trailing.len() > 1 << 30
            || f.stream.blocks.len() > n + (1 << 20)
        {
            return Err(Error::Invalid("beyond the limits of a record"));
        }
        let secs = t0.elapsed().as_secs_f64();
        Ok(Parsed {
            raw,
            f,
            streamed,
            secs,
            chosen: Default::default(),
        })
    }

    /// Bytes of the stream (without what followed it, for [`Parsed::head`]).
    pub fn len(&self) -> usize {
        self.raw.len()
    }

    pub fn is_empty(&self) -> bool {
        self.raw.is_empty()
    }

    /// The decompressed data.
    pub fn data(&self) -> &[u8] {
        &self.f.data
    }

    fn chosen(&self, opts: &Options) -> Result<&Chosen> {
        if let Some(c) = self.chosen.get() {
            return Ok(c);
        }
        let t0 = std::time::Instant::now();
        let (headers, first_recipe) = headers(&self.f);
        let fp = fingerprint(&self.f, first_recipe.unwrap_or(u32::MAX));
        let t1 = std::time::Instant::now();
        let rule = choose_rule(&self.f, opts, fp)?;
        let secs = [(t1 - t0).as_secs_f64(), t1.elapsed().as_secs_f64()];
        Ok(self.chosen.get_or_init(|| Chosen {
            headers,
            fp,
            rule,
            secs,
        }))
    }

    /// About how many bytes the record of [`Parsed::deconstruct`] with `opts` takes: the
    /// blocks and headers as they will be, and the corrections as runs of tokens spread over
    /// the data cost under the rule (16 KiB first, then 8 KiB about 8 times per doubling of
    /// the data; as static codes: an adaptive profile codes them in less), scaled to the
    /// whole. What it computes is not computed again by `deconstruct`.
    pub fn estimate(&self, opts: &Options) -> Result<usize> {
        let c = self.chosen(opts)?;
        let mut w = BitWriter::new();
        write_blocks(&mut w, &self.f);
        let chunks = match &self.f.chunks[..] {
            [] => chunks_of(&self.f),
            chunks => chunks.to_vec(),
        };
        // one coder for all, as the segments share their codes
        let mut rec = tokens::Recorder::default();
        rec.count_only = true;
        let mut covered = 0;
        for k in chunks.iter().filter(|k| k.count > 0) {
            // the stored blocks within the run (those before its first token are before `pos`)
            let skips: Vec<(usize, usize)> = self
                .f
                .skips
                .iter()
                .filter(|s| s.0 > k.token && s.0 < k.token + k.count)
                .map(|&(at, len)| (at - k.token, len))
                .collect();
            let tokens = &self.f.tokens[k.kept..k.kept + k.count];
            let spans: usize = skips.iter().map(|s| s.1).sum::<usize>()
                + tokens.iter().map(|t| t.span()).sum::<usize>();
            let range = Range {
                first: 0,
                count: k.count,
                pos: k.pos,
                end: k.pos + spans,
                skips: &skips,
            };
            let from = if k.pos == 0 { 0 } else { k.pos + inflate::WARM };
            tokens::code_range(
                &mut Warm {
                    rec: &mut rec,
                    from,
                },
                &self.f.data,
                c.rule,
                &range,
                Some(tokens),
                None,
            )?;
            covered += (k.pos + spans).saturating_sub(from.max(k.pos));
        }
        // (no tokens at all: stored blocks only)
        let corrections = if covered == 0 {
            0.0
        } else {
            rec.cost() / 8.0 * self.f.data.len() as f64 / covered as f64
        };
        Ok(w_bits(&w).div_ceil(8) + c.headers.len() + corrections as usize)
    }

    /// Deconstructs the stream with `opts` (those it was parsed with), as
    /// [`deconstruct_hinted`] does.
    pub fn deconstruct(self, opts: &Options, hints: &Hints) -> Result<(Vec<u8>, Vec<u8>, Report)> {
        self.chosen(opts)?;
        let chosen = self.chosen.into_inner().expect("chosen");
        deconstruct_parsed(
            self.raw,
            self.f,
            self.streamed,
            self.secs,
            chosen,
            opts,
            hints,
        )
    }
}

/// A [`tokens::Recorder`] of the tokens from `from` on, those before only filling the tables
/// (what a run sampled in the middle of a stream starts with: the coding of a whole stream has
/// them filled there).
struct Warm<'a> {
    rec: &'a mut tokens::Recorder,
    from: usize,
}

impl tokens::Corr for Warm<'_> {
    fn token(
        &mut self,
        t: &mut dyn table::Cands,
        d: &[u8],
        p: usize,
        pred: Token,
        class: usize,
        actual: Option<Token>,
    ) -> Result<Token> {
        if p < self.from {
            return actual.ok_or(Error::Inconsistent);
        }
        self.rec.token(t, d, p, pred, class, actual)
    }
}

/// Bytes of data between the parts a [`Ticking`] tells of.
const TICK: usize = 1 << 20;

/// A coder of a segment telling of the data it has coded ([`Flat::tell`]) every [`TICK`]
/// bytes, and the rest at [`Ticking::end`].
struct Ticking<'a, C> {
    c: &'a mut C,
    f: &'a Flat,
    pass: Pass<'a>,
    from: usize,
    next: usize,
    end: usize,
}

impl<'a, C: tokens::Corr> Ticking<'a, C> {
    fn new(c: &'a mut C, f: &'a Flat, r: &Range, pass: Pass<'a>) -> Self {
        Ticking {
            c,
            f,
            pass,
            from: r.pos,
            next: r.pos + TICK,
            end: r.end,
        }
    }

    fn end(self) {
        self.f.tell(self.from..self.end, self.pass);
    }
}

impl<C: tokens::Corr> tokens::Corr for Ticking<'_, C> {
    #[inline(always)]
    fn token(
        &mut self,
        t: &mut dyn table::Cands,
        d: &[u8],
        p: usize,
        pred: Token,
        class: usize,
        actual: Option<Token>,
    ) -> Result<Token> {
        if p >= self.next {
            self.f.tell(self.from..p, self.pass);
            (self.from, self.next) = (p, p + TICK);
        }
        self.c.token(t, d, p, pred, class, actual)
    }

    #[inline(always)]
    fn predict(&mut self, d: &[u8], p: usize, actual: Option<Token>) -> Option<Token> {
        self.c.predict(d, p, actual)
    }

    #[inline(always)]
    fn commit(&mut self, p: usize, tok: Token) {
        self.c.commit(p, tok)
    }

    #[inline(always)]
    fn skip(&mut self, end: usize) {
        self.c.skip(end)
    }

    #[inline(always)]
    fn lookahead(&self) -> bool {
        self.c.lookahead()
    }

    #[inline(always)]
    fn next_len(&mut self, len: usize) {
        self.c.next_len(len)
    }

    const SPECIALIZED: bool = C::SPECIALIZED;
}

/// The runs of tokens that [`inflate::parse_segments_head`] would keep, of a stream parsed
/// with all of them.
fn chunks_of(f: &Flat) -> Vec<inflate::Chunk> {
    let mut chunks = vec![inflate::Chunk {
        token: 0,
        kept: 0,
        pos: 0,
        count: 0,
    }];
    let (mut turn, mut next, mut keeping) = (
        ESTIMATE_FIRST,
        inflate::next_chunk(0).max(ESTIMATE_FIRST),
        true,
    );
    let (mut p, mut skips) = (0, f.skips.iter().peekable());
    for (k, t) in f.tokens.iter().enumerate() {
        while let Some(&&(at, len)) = skips.peek()
            && at == k
        {
            p += len;
            skips.next();
        }
        while p >= turn {
            if keeping {
                let c = chunks.last_mut().expect("a run");
                c.count = k - c.kept;
                (keeping, turn) = (false, next);
            } else {
                chunks.push(inflate::Chunk {
                    token: k,
                    kept: k,
                    pos: p,
                    count: 0,
                });
                (keeping, turn, next) = (true, p + inflate::CHUNK, inflate::next_chunk(p));
            }
        }
        p += t.span();
    }
    if keeping {
        let c = chunks.last_mut().expect("a run");
        c.count = f.tokens.len() - c.kept;
    }
    chunks
}

fn deconstruct_parsed(
    raw: &[u8],
    f: Flat,
    streamed: bool,
    secs_parse: f64,
    chosen: Chosen,
    opts: &Options,
    hints: &Hints,
) -> Result<(Vec<u8>, Vec<u8>, Report)> {
    let Chosen {
        headers,
        fp,
        rule,
        secs: [secs_headers, secs_rule],
    } = chosen;
    let threads = opts.threads.max(1);
    // (the coding of the data told of as that of the stream)
    let _watch = aqr_common::progress::alias(&f.data, raw);
    let profile = opts.profile;
    let catalog = &profile_catalog(profile)?.0;
    let adaptive = profile >= ADAPTIVE;
    let priors = match &opts.priors {
        Some(p) if adaptive => Some(p),
        _ if adaptive => Some(profile_priors(profile)?),
        _ => None,
    };
    let ranges = segment_ranges(&f);
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
        tokens: f.stats.cuts.last().copied().unwrap_or(0),
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
        let segs = if streamed {
            code_streamed(raw, &f, rule, &ranges, threads)?
        } else {
            par_map(ranges.len(), threads, |i| -> Result<tokens::Recorder> {
                let mut rec = tokens::Recorder::default();
                let r = &ranges[i];
                let actual = &f.tokens[r.first..r.first + r.count];
                let mut t = Ticking::new(&mut rec, &f, r, (0, &ONCE));
                tokens::code_range(&mut t, &f.data, rule, r, Some(actual), None)?;
                t.end();
                Ok(rec)
            })
            .into_iter()
            .collect::<Result<Vec<_>>>()?
        };
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

    report.times = [
        secs_parse,
        secs_rule,
        (t3 - t2).as_secs_f64(),
        secs_headers,
        t3.elapsed().as_secs_f64(),
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
        let segs = code_adaptive(
            f,
            rule,
            Some(p),
            None,
            priors,
            &zr,
            threads,
            collect,
            (0, &ONCE),
        )?;
        if segs.iter().all(|s| s.misses == 0) {
            // In a short stream the rule may predict as well, without zlib's parameters (ties
            // go to the rule, as below).
            if f.data.len() < HINT_RULE_BELOW {
                let alone = code_adaptive(
                    f,
                    rule,
                    None,
                    None,
                    priors,
                    &ranges,
                    threads,
                    collect,
                    (0, &ONCE),
                )?;
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
    // The rule alone, the chain rule and zlib's parameters are worked out independently, at
    // once given threads.
    // (told of as the first of two passes while zlib's parameters may be found: the stream is
    // coded again with them)
    let passes = std::sync::atomic::AtomicUsize::new(if stream.is_some() { 2 } else { 1 });
    let alone = || {
        code_adaptive(
            f,
            rule,
            None,
            None,
            priors,
            &ranges,
            threads,
            collect,
            (0, &passes),
        )
    };
    let chain = || -> Result<Option<(Rule, Vec<AdaptiveSeg>)>> {
        if !opts.chain {
            return Ok(None);
        }
        let Some((chain, coded)) = chain_rule(f, priors, &ranges, collect)? else {
            return Ok(None);
        };
        let segs = match coded {
            Some(segs) => segs,
            None => code_adaptive(
                f,
                chain,
                None,
                None,
                priors,
                &ranges,
                threads,
                collect,
                (0, &passes),
            )?,
        };
        Ok(Some((chain, segs)))
    };
    let params = || {
        let p = stream.as_ref().and_then(|s| zlib_params(s, &f.data, hints));
        if p.is_none() {
            passes.store(1, std::sync::atomic::Ordering::Relaxed);
        }
        p
    };
    let (alone, chain, params) = if threads > 1 {
        fn join<T>(h: aqr_common::pool::Handle<'_, T>) -> T {
            h.join().unwrap_or_else(|e| std::panic::resume_unwind(e))
        }
        // (on as many workers as there are, the rest here)
        aqr_common::pool::scope(2, |s| match s.workers() {
            0 => (alone(), chain(), params()),
            1 => {
                let c = s.spawn(chain);
                let (a, p) = (alone(), params());
                (a, join(c), p)
            }
            _ => {
                let (c, p) = (s.spawn(chain), s.spawn(params));
                let a = alone();
                (a, join(c), join(p))
            }
        })
    } else {
        (alone(), chain(), params())
    };
    let mut best = AdaptiveChoice {
        rule,
        zlib: None,
        segs: alone?,
        ranges,
    };
    if let Some((chain, segs)) = chain?
        && size(&segs) < size(&best.segs)
    {
        (best.rule, best.segs) = (chain, segs);
    }
    let Some((p, followed)) = params else {
        return Ok(best);
    };
    let ranges = zlib_ranges(f, p, &best.ranges);
    let segs = code_adaptive(
        f,
        best.rule,
        Some(p),
        followed,
        priors,
        &ranges,
        threads,
        collect,
        (1, &passes),
    )?;
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
const _: () = assert!(CHAIN_CONFIGS.len() <= table::MULTI);
/// No chain searches deeper than this quantile of the depths at which the stream's matches
/// are (the rest are coded by rank anyway): joining stays fast, and the intermediate depths
/// this gives often predict better too.
const DEPTH_QUANTILE: f64 = 0.995;
/// Chain configurations are compared on this much data when the stream has twice as much.
const CHAIN_PREFIX: usize = 256 << 10;

/// The best chain rule for a stream, by adaptive cost: of [`CHAIN_CONFIGS`] with the depth
/// capped by [`DEPTH_QUANTILE`], compared on a prefix of long streams. `None` without
/// matches. When they are compared on the whole stream and it is a single segment of
/// `ranges`, the segment as [`code_adaptive`] codes it comes along.
fn chain_rule<'a>(
    f: &'a Flat,
    priors: &Priors,
    ranges: &[Range<'a>],
    collect: bool,
) -> Result<Option<(Rule, Option<Vec<AdaptiveSeg>>)>> {
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
    let mut m = crate::matchfind::Matcher::single(&f.data);
    let dq = m.depth_quantile(&f.data, matches.into_iter(), DEPTH_QUANTILE);
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
        return Ok(Some((rules[0], None)));
    }
    // The first of the cheapest.
    let pick = |costs: &mut dyn Iterator<Item = f64>| {
        let mut best = (f64::INFINITY, 0);
        for (i, c) in costs.enumerate() {
            if c < best.0 {
                best = (c, i);
            }
        }
        best.1
    };
    if f.data.len() < 2 * CHAIN_PREFIX
        && let [r] = ranges
        && !collect
        && (r.first, r.count, r.pos, r.end) == (0, f.tokens.len(), 0, f.data.len())
    {
        let mut cs: Vec<_> = rules
            .iter()
            .map(|_| {
                adapt::Adaptive::new((adapt::Enc::new(), adapt::Cost::default()), priors, None)
            })
            .collect();
        // The chains just numbered serve again (the matcher is the same as for the range).
        tokens::code_chains(&mut cs, &f.data, &rules, r, &f.tokens, Some(m))?;
        let i = pick(&mut cs.iter().map(|a| a.b.1.0));
        let a = cs.swap_remove(i);
        let seg = AdaptiveSeg {
            cp: None,
            misses: a.misses,
            seen: vec![],
            body: a.b.0.finish(),
        };
        return Ok(Some((rules[i], Some(vec![seg]))));
    }
    let prefix = if f.data.len() >= 2 * CHAIN_PREFIX {
        CHAIN_PREFIX
    } else {
        usize::MAX
    };
    let p = Prepared::from_flat(f, prefix);
    let mut cs: Vec<_> = rules
        .iter()
        .map(|_| adapt::Adaptive::new(adapt::Cost::default(), priors, None))
        .collect();
    p.code_chains(&rules, &mut cs)?;
    let i = pick(&mut cs.iter().map(|a| a.b.0));
    Ok(Some((rules[i], None)))
}

/// zlib's parameters among those `hints` allows, if the stream looks like zlib's (see
/// [`crate::detect`]), and the emulation that found them, if at hand.
fn zlib_params<'a>(
    stream: &Stream,
    data: &'a [u8],
    hints: &Hints,
) -> Option<(zlib::Params, Option<Followed<'a>>)> {
    let candidates = crate::detect::candidates_hinted(stream, hints);
    if candidates.is_empty() {
        return None;
    }
    crate::detect::detect_refined_followed(stream, data, candidates).map(|(d, f)| (d.params, f))
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
/// there (and was not lost on the way); segments are then coded one after another. A single
/// segment is coded with the emulation `followed` gives, if any (from the parameters'
/// detection), rather than a new one. How far it is is told as pass `pass.0` of `pass.1`.
#[allow(clippy::too_many_arguments)]
fn code_adaptive<'a>(
    f: &'a Flat,
    rule: Rule,
    zlib: Option<zlib::Params>,
    followed: Option<Followed<'a>>,
    priors: &Priors,
    ranges: &[Range],
    threads: usize,
    collect: bool,
    pass: Pass,
) -> Result<Vec<AdaptiveSeg>> {
    // Segment `i`, coded with `a` (from checkpoint `cp` if not the first).
    let finish = |mut a: adapt::Adaptive<'a, adapt::Enc>, i: usize, cp: Option<Checkpoint>| {
        let r = &ranges[i];
        if collect {
            a.seen = Some(vec![[0; 2]; adapt::NUM]);
        }
        let actual = &f.tokens[r.first..r.first + r.count];
        let mut t = Ticking::new(&mut a, f, r, pass);
        tokens::code_range(&mut t, &f.data, rule, r, Some(actual), None)?;
        t.end();
        let emu = a.take_emulator();
        let seg = AdaptiveSeg {
            cp,
            misses: a.misses,
            seen: a.seen.take().unwrap_or_default(),
            body: a.b.finish(),
        };
        Ok::<_, Error>((seg, emu))
    };
    let code = |i: usize, cp: Option<Checkpoint>, emu: Option<Emulator<'a>>| {
        finish(adapt::Adaptive::new(adapt::Enc::new(), priors, emu), i, cp)
    };
    let Some(p) = zlib else {
        return par_map(ranges.len(), threads, |i| code(i, None, None).map(|r| r.0))
            .into_iter()
            .collect();
    };
    if let (Some(followed), [_]) = (followed, ranges) {
        debug_assert_eq!(followed.emu.params(), p);
        let a = adapt::Adaptive::following(adapt::Enc::new(), priors, followed);
        return Ok(vec![finish(a, 0, None)?.0]);
    }
    let mut emu = Some(Emulator::new(&f.data, p));
    let mut out = Vec::with_capacity(ranges.len());
    for (i, r) in ranges.iter().enumerate() {
        let mut cp = None;
        if i > 0 {
            cp = emu
                .as_ref()
                .and_then(Emulator::checkpoint)
                .filter(|cp| cp.cursor - cp.held as usize == r.pos);
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
    let (s, b) = (cp.cursor as u64, cp.origin as u64);
    w.bits(1, cp.held as u32);
    put_gamma(w, s / ws - b / ws);
    put_gamma(w, b + 2 * ws - cp.fed as u64);
    put_gamma(w, 2 * ws - cp.zero_mark as u64);
    put_gamma(w, cp.length as u64 - 2);
    if cp.length > 2 {
        put_gamma(w, s - cp.source as u64);
    }
}

/// Inverse of [`put_checkpoint`] for a segment starting at `pos` in `n` bytes of data.
fn get_checkpoint(r: &mut BitReader, pos: usize, wbits: u8, n: usize) -> Result<Checkpoint> {
    let ws = 1usize << wbits;
    let held = r.bits(1)? == 1;
    let cursor = pos + held as usize;
    let origin = (cursor / ws)
        .checked_sub(get_usize(r, n)?)
        .ok_or(Error::Corrupt)?
        * ws;
    let fed = (origin + 2 * ws)
        .checked_sub(get_usize(r, 2 * ws)?)
        .ok_or(Error::Corrupt)?;
    let zero_mark = (2 * ws)
        .checked_sub(get_usize(r, 2 * ws)?)
        .ok_or(Error::Corrupt)?;
    let length = 2 + get_usize(r, crate::matchfind::MAX_MATCH)?;
    let source = if length > 2 {
        cursor
            .checked_sub(get_usize(r, ws)?)
            .ok_or(Error::Corrupt)?
    } else {
        0
    };
    Ok(Checkpoint {
        cursor,
        origin,
        fed,
        zero_mark,
        held,
        length,
        source,
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
        let f = Flat::new(raw, usize::MAX, 1, false, usize::MAX)?;
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

    /// Codes the tokens under several [`tokens::Kind::Chain`] rules at once, one coder each
    /// ([`tokens::code_chains`]).
    fn code_chains<C: tokens::Corr>(&self, rules: &[Rule], cs: &mut [C]) -> Result<()> {
        let range = Range {
            first: 0,
            count: self.tokens.len(),
            pos: 0,
            end: self.data.len(),
            skips: &self.skips,
        };
        tokens::code_chains(cs, &self.data, rules, &range, &self.tokens, None)
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
    let f = Flat::new(raw, usize::MAX, 1, false, usize::MAX)?;
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
    let mut out = Vec::with_capacity(data.len() / 2 + 64);
    reconstruct_into(data, meta, model, threads, &mut |b| {
        out.extend_from_slice(b)
    })?;
    Ok(out)
}

/// [`reconstruct_with`], the stream given to `sink` in pieces, in order, as it is written:
/// segments are decoded a batch at a time on the threads, and each block written once its
/// tokens are all decoded, its tokens then dropped. What is held at once is a batch of
/// segments' tokens and the largest block's, however long the stream.
pub fn reconstruct_into(
    data: &[u8],
    meta: &[u8],
    model: ModelData,
    threads: usize,
    sink: &mut dyn FnMut(&[u8]),
) -> Result<()> {
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

    let decode = |i: usize| -> Result<(Vec<Token>, usize)> {
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
    };

    let hr = &mut BitReader::new(hdrs);
    let mut recipe = header::Recipe::default();
    let mut w = write::Writer::new((data.len() / 2).min(1 << 20) + 64);
    let nb = layout.blocks.len();
    let mut shapes = layout.blocks.into_iter();
    // tokens decoded (from token `base` on), the next block (from token `k` on)
    let (mut pending, mut base, mut decoded) = (Vec::<Token>::new(), 0usize, 0usize);
    let (mut bi, mut k) = (0usize, 0usize);
    // where the last segment's data ends
    let mut end = 0;
    let batch = 4 * threads;
    // (as tasks on WebAssembly)
    let decode_batch = |s0: usize| {
        let s1 = (s0 + batch).min(segs.len());
        (
            s0,
            aqr_common::pool::par_map_apart(s1 - s0, threads, |j| decode(s0 + j)),
        )
    };
    // the next batch is decoded while the blocks of this one are written (before them, on a
    // single thread)
    enum Batch<'s, T> {
        Done(T),
        Running(aqr_common::pool::Handle<'s, T>),
    }
    aqr_common::pool::scope((threads > 1) as usize, |scope| -> Result<()> {
        let start = |s0: usize| {
            if scope.workers() > 0 {
                Batch::Running(scope.spawn(move || decode_batch(s0)))
            } else {
                Batch::Done(decode_batch(s0))
            }
        };
        let mut next = (!segs.is_empty()).then(|| start(0));
        loop {
            let more = next.is_some();
            if let Some(h) = next.take() {
                let (s0, results) = match h {
                    Batch::Done(b) => b,
                    Batch::Running(h) => h.join().map_err(|_| Error::Corrupt)?,
                };
                let s1 = s0 + results.len();
                if s1 < segs.len() {
                    next = Some(start(s1));
                }
                for (i, r) in (s0..s1).zip(results) {
                    let (t, p) = r?;
                    if p != segs.get(i + 1).map_or(p, |s| s.3) {
                        return Err(Error::Corrupt);
                    }
                    decoded += t.len();
                    pending.extend_from_slice(&t);
                    end = p;
                }
            }
            // the blocks whose tokens are all decoded, a few thousand at a time: their symbols
            // counted on the threads, their headers read in order, and then they are written
            // on the threads
            while bi < nb && (k + counts[bi] <= decoded || !more) {
                let mut ready = vec![];
                while ready.len() < 1 << 12 && bi < nb && (k + counts[bi] <= decoded || !more) {
                    let c = counts[bi];
                    pending.get(k - base..k - base + c).ok_or(Error::Corrupt)?;
                    ready.push((shapes.next().unwrap(), k - base..k - base + c));
                    k += c;
                    bi += 1;
                }
                let freqs = par_map(ready.len(), threads, |j| match ready[j] {
                    (Shape::Coded { btype: 1, .. }, _) | (Shape::Stored { .. }, _) => None,
                    (Shape::Coded { .. }, ref r) => Some(header::freqs(&pending[r.clone()])),
                });
                let first = bi - ready.len();
                let mut blocks = Vec::with_capacity(ready.len());
                for (j, ((shape, r), f)) in ready.into_iter().zip(freqs).enumerate() {
                    let body = match (shape, f) {
                        (Shape::Stored { pad, len }, _) => Body::Stored { pad, len },
                        (_, Some((lf, df))) => Body::Dynamic {
                            header: header::read_freqs(hr, &lf, &df, &mut recipe)?,
                            tokens: vec![],
                        },
                        _ => Body::Fixed { tokens: vec![] },
                    };
                    blocks.push((
                        Block {
                            last: first + j + 1 == nb,
                            body,
                        },
                        r,
                    ));
                }
                w.blocks(&blocks, &pending, data, threads)
                    .map_err(|_| Error::Corrupt)?;
            }
            sink(&w.take());
            if k - base > pending.len() / 2 {
                pending.drain(..k - base);
                base = k;
            }
            if !more {
                return Ok(());
            }
        }
    })?;
    // Stored blocks after the last token cover the rest of the data.
    end += skips
        .iter()
        .filter(|s| s.0 == total)
        .map(|s| s.1)
        .sum::<usize>();
    if end != n || bi != nb {
        return Err(Error::Corrupt);
    }
    sink(
        &w.finish(layout.tail_pad, &layout.trailing)
            .map_err(|_| Error::Corrupt)?,
    );
    Ok(())
}

/// `f(0..n)` on up to `threads` threads, in order.
fn par_map<R: Send>(n: usize, threads: usize, f: impl Fn(usize) -> R + Sync) -> Vec<R> {
    aqr_common::pool::par_map(n, threads, f)
}
