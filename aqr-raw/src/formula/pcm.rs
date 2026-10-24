//! The module `pcm-recons`: interleaved PCM samples from their coded streams
//! ([`crate::audio::pcm`]).
//!
//! Params: the sample format (3 bytes, [`Format::to_bytes`]). Resources: the residual model's
//! frequency tables. Inputs: the three byte streams (channel assignments, subframe
//! parameters, warm-up samples), the rANS words (u32 LE) and the two raw bit streams.

use super::{Error, Result};
use crate::audio::pcm::{self, Format, N_BYTE_STREAMS};
use crate::audio::residual::{self, Coded};
use aqr_formula::{Formula, Kind, Module, Node, Resource};
use std::sync::OnceLock;

pub const MODULE: Module = Module { name: "pcm-recons", version: 1 };

/// Classes of the input slots, and their kinds: the rANS words and the raw bits do not
/// compress.
const CLASSES: [(&str, Kind); N_BYTE_STREAMS + 3] = [
    ("pcm/blocks", Kind::Unknown),
    ("pcm/subframes", Kind::Unknown),
    ("pcm/warmup", Kind::Unknown),
    ("pcm/rans", Kind::Compressed),
    ("pcm/raw0", Kind::Compressed),
    ("pcm/raw1", Kind::Compressed),
];

/// The residual model's resource: the built-in frequency tables, uncompressed (shared with
/// whatever else codes residuals with them, as aqr-flac's module does).
pub fn resource() -> Resource {
    static R: OnceLock<Resource> = OnceLock::new();
    R.get_or_init(|| Resource::new(residual::builtin_raw())).clone()
}

/// The application giving `samples` (whole sample frames of `fmt`), its inputs pushed to `f`.
/// None for no samples, or where the coder panics.
pub fn apply(f: &mut Formula, samples: &[u8], fmt: Format, threads: usize) -> Option<Node> {
    if samples.is_empty() {
        return None;
    }
    let c = std::panic::catch_unwind(|| pcm::encode_threads(samples, fmt, threads)).ok()??;
    let words = c.words.iter().flat_map(|w| w.to_le_bytes()).collect();
    let [raw0, raw1] = c.raw;
    let slots = c.streams.into_iter().chain([words, raw0, raw1]);
    let inputs = slots.zip(CLASSES).map(|(s, (class, kind))| f.push_as(class, kind, s)).collect();
    Some(Node::apply_with(MODULE, vec![resource()], fmt.to_bytes().to_vec(), inputs, samples.len() as u64))
}

/// The module: the `len` bytes of samples from `params` (their format), `resources` (the
/// tables) and `inputs` (the streams).
pub fn decode(params: &[u8], resources: &[&[u8]], inputs: &[&[u8]], len: u64, threads: usize) -> Result<Vec<u8>> {
    let corrupt = |s: &str| Error(format!("corrupt: {s}"));
    let fmt = Format::from_bytes(params).ok_or_else(|| corrupt("params"))?;
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
    pcm::decode_threads(&c, fmt, &t, len as usize, &mut out, threads).map_err(|e| corrupt(e.0))?;
    if out.len() as u64 != len {
        return Err(corrupt("length"));
    }
    Ok(out)
}
