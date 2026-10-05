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
pub mod huffman;
pub mod matchfind;
pub mod model;
pub mod predict;

use std::fmt;

pub use coder::{Coder, Decision, Metadata};
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
}

impl Default for Options<'static> {
    fn default() -> Self {
        let priors = Priors::builtin();
        Options {
            configs: priors.catalog.clone(),
            priors,
        }
    }
}

/// A parsed stream ready to be coded under different configurations.
pub struct Parsed {
    pub stream: deflate::Stream,
    pub data: Vec<u8>,
}

impl Parsed {
    pub fn new(raw: &[u8]) -> Result<Parsed> {
        let (stream, data) = deflate::parse(raw)?;
        // The writer must round-trip what the parser saw, or nothing downstream can.
        if deflate::write(&stream, &data)? != raw {
            return Err(Error::Inconsistent);
        }
        Ok(Parsed { stream, data })
    }

    /// Codes the stream into `c` under `cfg`.
    pub fn code<C: Coder>(&self, c: &mut C, cfg: Config) -> Result<()> {
        let (coded, _) = predict::code_stream(c, &self.data, Some(&self.stream), Some(cfg))?;
        if coded != self.stream {
            return Err(Error::Inconsistent);
        }
        Ok(())
    }

    /// Estimated metadata size in bits under `cfg`.
    pub fn cost(&self, cfg: Config, priors: &Priors) -> Result<f64> {
        let mut c = model::CostCoder::new(priors);
        self.code(&mut c, cfg)?;
        Ok(c.total())
    }

    /// The cheapest of `configs`.
    pub fn best_config(&self, configs: &[Config], priors: &Priors) -> Result<Config> {
        if configs.len() == 1 {
            return Ok(configs[0]);
        }
        let mut best = None;
        for &cfg in configs {
            let cost = self.cost(cfg, priors)?;
            if best.is_none_or(|(c, _)| cost < c) {
                best = Some((cost, cfg));
            }
        }
        best.map(|(_, cfg)| cfg)
            .ok_or(Error::Param("no predictor configuration".into()))
    }
}

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
    let cfg = parsed.best_config(&opts.configs, opts.priors)?;
    parsed.code(c, cfg)?;
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
    let (stream, _) = predict::code_stream(c, data, None, None)?;
    deflate::write(&stream, data).map_err(|_| Error::Corrupt)
}
