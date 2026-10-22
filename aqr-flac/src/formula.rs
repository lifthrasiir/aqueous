//! aqr-flac as a module of entry formulas (see aqr-formula): the coded streams as input
//! slots.
//!
//! Params: one byte, 0 for FLAC and 1 for PCM (WAV, AIFF and raw samples decode alike).
//! Inputs: the six byte streams (`codec::STREAM_NAMES`), the rANS words (u32 LE) and the two
//! raw bit streams. FORMAT.md describes the module in full.

use crate::codec::{self, Coded, N_BYTE_STREAMS};
use crate::rans::Tables;
use crate::residual;
use crate::{flac, pcm};
use aqr_formula::{Formula, Module, Node, Resource};
use std::sync::OnceLock;

pub const MODULE: Module = Module { name: "flac-recons", version: 2 };

#[derive(Debug)]
pub struct Error(pub String);

pub type Result<T> = std::result::Result<T, Error>;

/// Classes of the input slots.
const CLASSES: [&str; N_BYTE_STREAMS + 3] = [
    "flac/prefix",
    "flac/layout",
    "flac/junk",
    "flac/headers",
    "flac/subframes",
    "flac/warmup",
    "flac/rans",
    "flac/raw0",
    "flac/raw1",
];

/// What the input is: FLAC, WAV / AIFF (both coded as PCM), or raw PCM (also anything
/// unrecognised, which is not coded).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Flac,
    Wav,
    Pcm,
}

impl Kind {
    /// The kind [`encode_coded`] codes the input as.
    fn of(input: &[u8], opt: &Options) -> Kind {
        if opt.raw.is_some() {
            Kind::Pcm
        } else if flac::parse_metadata(input).is_some() {
            Kind::Flac
        } else if pcm::detect(input).is_some() {
            Kind::Wav
        } else {
            Kind::Pcm
        }
    }
}

/// Options of [`encode_with`].
#[derive(Clone, Copy, Debug)]
pub struct Options {
    /// Take the whole input as headerless PCM of this format.
    pub raw: Option<pcm::Format>,
    /// Threads (1: all on the calling thread). The output does not depend on it.
    pub threads: usize,
}

impl Default for Options {
    fn default() -> Self {
        Options { raw: None, threads: 1 }
    }
}

/// Encodes FLAC, or WAV / AIFF (detected), or headerless PCM if `opt.raw` is given.
/// (`kind` is [`Kind::of`] the input.)
fn encode_coded(input: &[u8], opt: &Options, kind: Kind) -> Option<Coded> {
    let t = opt.threads;
    match (kind, opt.raw) {
        (_, Some(fmt)) => pcm::encode_threads(input, fmt, 0, input.len(), t),
        (Kind::Flac, None) => codec::encode_threads(input, t),
        (Kind::Wav, None) => {
            let (fmt, start, len) = pcm::detect(input)?;
            pcm::encode_threads(input, fmt, start, len, t)
        }
        (Kind::Pcm, None) => None,
    }
}

fn decode_coded(
    kind: Kind,
    c: &Coded,
    t: &Tables,
    len: usize,
    out: &mut Vec<u8>,
    threads: usize,
) -> std::result::Result<(), codec::Error> {
    if kind == Kind::Flac {
        codec::decode_threads(c, t, len, out, threads)
    } else {
        pcm::decode_threads(c, t, len, out, threads)
    }
}

/// [`encode_with`] with the default options (kind detected) and `threads`.
pub fn encode(input: &[u8], threads: usize) -> Option<Formula> {
    encode_with(input, &Options { threads, ..Options::default() })
}

/// The formula of `input` (FLAC, WAV / AIFF, or headerless PCM if `opt.raw` is given). None
/// where the input is not coded, or the coder panics.
pub fn encode_with(input: &[u8], opt: &Options) -> Option<Formula> {
    let (kind, c) = std::panic::catch_unwind(|| {
        let kind = Kind::of(input, opt);
        encode_coded(input, opt, kind).map(|c| (kind, c))
    })
    .ok()??;
    let mut f = Formula::default();
    let words = c.words.iter().flat_map(|w| w.to_le_bytes()).collect();
    let [raw0, raw1] = c.raw;
    let slots = c.streams.into_iter().chain([words, raw0, raw1]);
    let inputs = slots.zip(CLASSES).map(|(s, class)| f.push(class, s)).collect();
    f.root = Node::apply_with(MODULE, vec![resource()], vec![(kind != Kind::Flac) as u8], inputs, input.len() as u64);
    Some(f)
}

/// The module's resource: the built-in frequency tables, uncompressed.
pub fn resource() -> Resource {
    static R: OnceLock<Resource> = OnceLock::new();
    R.get_or_init(|| Resource::new(residual::builtin_raw())).clone()
}

/// The module: the output of `len` bytes from `params` (the kind), `resources` (the
/// tables) and `inputs` (the streams).
pub fn decode(params: &[u8], resources: &[&[u8]], inputs: &[&[u8]], len: u64, threads: usize) -> Result<Vec<u8>> {
    let corrupt = |s: &str| Error(format!("corrupt: {s}"));
    let kind = match params {
        [0] => Kind::Flac,
        [1] => Kind::Pcm,
        _ => return Err(corrupt("params")),
    };
    let [res] = resources else { return Err(corrupt("resources")) };
    let t = residual::tables_from(res).ok_or_else(|| corrupt("tables"))?;
    let [streams @ .., words, raw0, raw1] = inputs else { return Err(corrupt("inputs")) };
    if streams.len() != N_BYTE_STREAMS || words.len() % 4 != 0 {
        return Err(corrupt("inputs"));
    }
    // No output is larger than 64 times its coded form.
    let coded_len = inputs.iter().map(|i| i.len() as u64).sum::<u64>();
    if len > coded_len.saturating_mul(64) + (1 << 20) {
        return Err(Error("length".into()));
    }
    let c = Coded {
        streams: streams.iter().map(|s| s.to_vec()).collect(),
        words: words.chunks_exact(4).map(|w| u32::from_le_bytes(w.try_into().unwrap())).collect(),
        raw: [raw0.to_vec(), raw1.to_vec()],
    };
    let mut out = Vec::new();
    decode_coded(kind, &c, &t, len as usize, &mut out, threads).map_err(|e| corrupt(e.0))?;
    if out.len() as u64 != len {
        return Err(corrupt("length"));
    }
    Ok(out)
}
