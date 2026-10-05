//! Deconstructs raw DEFLATE streams into decompressed data plus a stream of predictable
//! decisions, and reconstructs the bit-exact original from the two.
//!
//! ```no_run
//! # let raw: Vec<u8> = vec![];
//! let (data, meta) = flate_recons::deconstruct(&raw)?;
//! assert_eq!(flate_recons::reconstruct(&data, &meta)?, raw);
//! // Reference entropy coding of the metadata:
//! let priors = flate_recons::Priors::builtin();
//! let packed = flate_recons::model::pack(&meta, priors);
//! assert_eq!(flate_recons::model::unpack(&data, &packed, priors)?, raw);
//! # Ok::<(), flate_recons::Error>(())
//! ```
//!
//! The metadata ([`Metadata`]) is a list of binary decisions, each tagged with a context id
//! ([`ctx::Ctx`]) and split into channels ([`ctx::Channel`]). It is meant to be compressed
//! separately by a context-adaptive binary arithmetic coder; [`model::ContextModel`] gives
//! trained adaptive probabilities for that, and [`model::pack`] is a complete reference
//! coder. For streaming into or out of an external coder use [`deconstruct_into`] and
//! [`reconstruct_with`] with your own [`Coder`].

pub mod bits;
pub mod coder;
pub mod ctx;
pub mod deflate;
pub mod detect;
pub mod huffman;
pub mod matchfind;
pub mod model;
pub mod predict;
pub mod zlib;

use std::fmt;

pub use coder::{Coder, Decision, Metadata};
pub use detect::Hints;
pub use model::Priors;
pub use predict::Config;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The DEFLATE stream ended prematurely.
    Truncated,
    /// The DEFLATE stream is not valid.
    Invalid(&'static str),
    /// The metadata does not describe a valid stream for the given data.
    Corrupt,
    /// The predictor did not reproduce its input; a bug.
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

#[derive(Debug, Clone)]
pub struct Options<'a> {
    /// Predictor configurations to try; the cheapest one under `priors` is used.
    pub configs: Vec<Config>,
    pub priors: &'a Priors,
    /// What the stream's container says about its encoder (e.g. [`Hints::zlib_header`]).
    pub hints: Hints,
    /// Whether to try zlib emulation at all.
    pub zlib: bool,
}

impl Default for Options<'static> {
    fn default() -> Self {
        let priors = Priors::builtin();
        Options {
            configs: priors.catalog.clone(),
            priors,
            hints: Hints::default(),
            zlib: true,
        }
    }
}

/// A parsed stream ready to be coded under different configurations. Match searches are
/// cached across calls.
pub struct Parsed {
    pub stream: deflate::Stream,
    pub data: Vec<u8>,
    matcher: std::sync::Mutex<Option<matchfind::Matcher>>,
}

impl Parsed {
    pub fn new(raw: &[u8]) -> Result<Parsed> {
        let (stream, data) = deflate::parse(raw)?;
        // The writer must round-trip what the parser saw, or nothing downstream can.
        if deflate::write(&stream, &data)? != raw {
            return Err(Error::Inconsistent);
        }
        Ok(Parsed {
            stream,
            data,
            matcher: Default::default(),
        })
    }

    /// Codes the stream into `c` under `cfg`.
    pub fn code<C: Coder>(&self, c: &mut C, cfg: Config) -> Result<()> {
        self.code_exact(c, cfg).map(|_| ())
    }

    /// [`Parsed::code`], returning whether zlib emulation reproduced every block.
    fn code_exact<C: Coder>(&self, c: &mut C, cfg: Config) -> Result<bool> {
        let mut m = self.matcher.lock().unwrap();
        let coded = predict::code_stream(c, &self.data, Some(&self.stream), Some(cfg), &mut m)?;
        if coded.stream != self.stream {
            return Err(Error::Inconsistent);
        }
        Ok(cfg.zlib.is_some() && coded.exact_blocks == self.stream.blocks.len())
    }

    /// The `q`-quantile of how deep in the exact candidate chains the stream's matches are.
    fn depth_quantile(&self, q: f64) -> usize {
        let mut m = self.matcher.lock().unwrap();
        let m = m.get_or_insert_with(|| matchfind::Matcher::new(&self.data));
        let mut pos = 0;
        let mut matches = vec![];
        for b in &self.stream.blocks {
            match &b.body {
                deflate::Body::Stored { len, .. } => pos += *len as usize,
                deflate::Body::Fixed { tokens } | deflate::Body::Dynamic { tokens, .. } => {
                    for &t in tokens {
                        if let deflate::Token::Match { dist, .. } = t {
                            matches.push((pos, dist as usize));
                        }
                        pos += t.span();
                    }
                }
            }
        }
        m.depth_quantile(matches.into_iter(), q)
    }

    /// The stream cut after (about) `bytes` of data, for ranking configurations cheaply: the
    /// block reaching that point loses its remaining tokens and becomes the last one. `None`
    /// if the stream is not much longer than that.
    pub fn prefix(&self, bytes: usize) -> Option<Parsed> {
        if self.data.len() < 2 * bytes {
            return None;
        }
        let mut blocks = vec![];
        let mut pos = 0;
        for b in &self.stream.blocks {
            let mut b = b.clone();
            match &mut b.body {
                deflate::Body::Stored { len, .. } => pos += *len as usize,
                deflate::Body::Fixed { tokens } | deflate::Body::Dynamic { tokens, .. } => {
                    let mut keep = 0;
                    while keep < tokens.len() && pos < bytes {
                        pos += tokens[keep].span();
                        keep += 1;
                    }
                    tokens.truncate(keep);
                }
            }
            let done = pos >= bytes;
            b.last = done;
            blocks.push(b);
            if done {
                break;
            }
        }
        let stream = deflate::Stream {
            blocks,
            tail_pad: 0,
            trailing: vec![],
        };
        Some(Parsed {
            stream,
            data: self.data[..pos].to_vec(),
            matcher: Default::default(),
        })
    }

    /// Estimated metadata size in bits under `cfg`.
    pub fn cost(&self, cfg: Config, priors: &Priors) -> Result<f64> {
        Ok(self
            .cost_within(cfg, priors, f64::INFINITY)?
            .unwrap_or(f64::INFINITY))
    }

    /// Like [`Parsed::cost`], but gives up with `None` once the cost exceeds `budget`.
    pub fn cost_within(&self, cfg: Config, priors: &Priors, budget: f64) -> Result<Option<f64>> {
        Ok(self.record_within(cfg, priors, budget)?.map(|(c, ..)| c))
    }

    /// Codes under `cfg`, returning the estimated cost and the decisions in coding order,
    /// or `None` once the cost exceeds `budget`.
    fn record_within(
        &self,
        cfg: Config,
        priors: &Priors,
        budget: f64,
    ) -> Result<Option<(f64, Vec<Decision>, bool)>> {
        struct Tee {
            cost: model::CostCoder,
            log: Vec<Decision>,
        }
        impl Coder for Tee {
            fn bit(&mut self, ctx: ctx::Ctx, bit: bool) -> bool {
                self.log.push(Decision { ctx, bit });
                self.cost.bit(ctx, bit)
            }
            fn failed(&self) -> bool {
                self.cost.failed()
            }
        }
        let mut c = Tee {
            cost: model::CostCoder::new(priors),
            log: vec![],
        };
        c.cost.budget = budget;
        match self.code_exact(&mut c, cfg) {
            Err(Error::Corrupt) if c.failed() => Ok(None),
            Err(e) => Err(e),
            Ok(exact) => Ok(Some((c.cost.total(), c.log, exact))),
        }
    }

    /// The cheapest of `configs`, after trying zlib emulation if the stream looks like zlib's
    /// output.
    pub fn best_config(&self, configs: &[Config], priors: &Priors) -> Result<Config> {
        Ok(self.best(configs, priors, Some(&Hints::default()))?.0)
    }

    /// Like [`Parsed::best_config`], also returning the decisions of the chosen configuration
    /// in coding order: every candidate is recorded while its cost is measured, so the winner
    /// need not be coded again.
    pub fn best(
        &self,
        configs: &[Config],
        priors: &Priors,
        hints: Option<&Hints>,
    ) -> Result<(Config, Vec<Decision>)> {
        let generic = configs.first().copied().unwrap_or_default();
        let zcfg = |p| Config {
            zlib: Some(p),
            ..generic
        };
        let mut best: Option<(f64, Config, Vec<Decision>)> = None;
        let mut tried = None;

        // zlib, first with its usual settings only, guessed from the first tokens: if that
        // reproduces the whole stream, it is the answer.
        // No hints: no zlib emulation at all.
        let zlib_like = hints.map_or(vec![], |h| detect::candidates_hinted(&self.stream, h));
        if let Some(d) =
            detect::first_exact(&self.stream, &self.data, detect::common(zlib_like.clone()))
        {
            let cfg = zcfg(d.params);
            if let Some((cost, log, exact)) = self.record_within(cfg, priors, f64::INFINITY)? {
                if exact {
                    return Ok((cfg, log));
                }
                best = Some((cost, cfg, log));
                tried = Some(d.params);
            }
        }
        let mut candidates = vec![];
        let mut zlib_late = None;
        if !zlib_like.is_empty()
            && let Some(d) = detect::detect_refined(&self.stream, &self.data, zlib_like)
            && Some(d.params) != tried
        {
            // Try the likely winner first, so that the others are cut off early.
            if d.misses * 100 <= d.tokens {
                candidates.push(zcfg(d.params));
            } else {
                zlib_late = Some(zcfg(d.params));
            }
        }

        // No generic configuration searches deeper than nearly all of the stream's matches
        // lie (the rest are coded by rank anyway): reconstruction stays fast, and the
        // intermediate depths this gives often predict better too.
        let dq = self.depth_quantile(DEPTH_QUANTILE);
        let cap = predict::DEPTHS
            .iter()
            .position(|&d| d as usize > dq)
            .unwrap_or(predict::DEPTHS.len() - 1) as u8;
        let clamped: Vec<Config> = configs
            .iter()
            .map(|c| Config {
                depth: c.depth.min(cap),
                ..*c
            })
            .collect();
        let configs = &clamped[..];

        // The generic configurations are ranked on a prefix of long streams; only the best
        // one is then coded in full.
        match self.prefix(PREFIX_BYTES).filter(|_| configs.len() > 1) {
            Some(pre) => {
                let mut top: Option<(f64, Config)> = None;
                for &cfg in configs {
                    let budget = top.map_or(f64::INFINITY, |t| t.0);
                    if let Some(c) = pre.cost_within(cfg, priors, budget)?
                        && top.is_none_or(|t| c < t.0)
                    {
                        top = Some((c, cfg));
                    }
                }
                candidates.extend(top.map(|t| t.1));
            }
            None => candidates.extend_from_slice(configs),
        }
        candidates.extend(zlib_late);

        // Costs only grow while coding, so a configuration can be abandoned as soon as it
        // exceeds the best one so far without changing the outcome.
        for &cfg in &candidates {
            let budget = best.as_ref().map_or(f64::INFINITY, |b| b.0);
            if let Some((cost, log, _)) = self.record_within(cfg, priors, budget)?
                && best.as_ref().is_none_or(|b| cost < b.0)
            {
                best = Some((cost, cfg, log));
            }
        }
        best.map(|(_, cfg, log)| (cfg, log))
            .ok_or(Error::Param("no predictor configuration".into()))
    }
}

/// Quantile of the matches' chain depths that bounds the generic search depth (measured on
/// the held-out samples: 0.99 costs 0.9% more metadata for 22% faster reconstruction, 0.995
/// gives both smaller metadata and 12% faster reconstruction).
const DEPTH_QUANTILE: f64 = 0.995;

/// Generic configurations are ranked on this much of a stream's data when it is at least
/// twice as long.
const PREFIX_BYTES: usize = 256 << 10;

/// Splits a raw DEFLATE stream into its decompressed data and reconstruction metadata.
pub fn deconstruct(raw: &[u8]) -> Result<(Vec<u8>, Metadata)> {
    deconstruct_with(raw, &Options::default())
}

pub fn deconstruct_with(raw: &[u8], opts: &Options) -> Result<(Vec<u8>, Metadata)> {
    let mut rec = coder::Recorder::default();
    let data = deconstruct_into(raw, opts, &mut rec)?;
    Ok((data, rec.meta))
}

/// Like [`deconstruct_with`], but sends the decisions to `c` (e.g. an arithmetic encoder).
pub fn deconstruct_into<C: Coder>(raw: &[u8], opts: &Options, c: &mut C) -> Result<Vec<u8>> {
    let parsed = Parsed::new(raw)?;
    let hints = opts.zlib.then_some(&opts.hints);
    let (_, log) = parsed.best(&opts.configs, opts.priors, hints)?;
    for d in log {
        c.bit(d.ctx, d.bit);
    }
    Ok(parsed.data)
}

/// Rebuilds the exact DEFLATE stream from the decompressed data and the metadata.
pub fn reconstruct(data: &[u8], meta: &Metadata) -> Result<Vec<u8>> {
    let mut p = coder::Player::new(meta);
    let out = reconstruct_with(data, &mut p)?;
    if !p.exhausted() {
        return Err(Error::Corrupt);
    }
    Ok(out)
}

/// Like [`reconstruct`], pulling decisions from `c` (e.g. an arithmetic decoder).
pub fn reconstruct_with<C: Coder>(data: &[u8], c: &mut C) -> Result<Vec<u8>> {
    let stream = predict::code_stream(c, data, None, None, &mut None)?.stream;
    deflate::write(&stream, data).map_err(|_| Error::Corrupt)
}
