//! aqr-flac as a module of entry formulas (see aqr-formula): the coded streams as input
//! slots.
//!
//! No params. Resources: aqr-raw's residual tables. Inputs: the six byte streams
//! (`codec::STREAM_NAMES`), the rANS words (u32 LE) and the two raw bit streams. FORMAT.md
//! describes the module in full. WAV, AIFF and headerless PCM are aqr-raw's (`pcm-recons`).

use crate::codec::{self, Coded, N_BYTE_STREAMS};
use aqr_formula::{Formula, Kind as StreamKind, Module, Node, Resource};
use aqr_raw::audio::residual;

pub const MODULE: Module = Module { name: "flac-recons", version: 3 };

#[derive(Debug)]
pub struct Error(pub String);

pub type Result<T> = std::result::Result<T, Error>;

/// Classes of the input slots, and their kinds: the rANS words and the raw bits do not
/// compress.
const CLASSES: [(&str, StreamKind); N_BYTE_STREAMS + 3] = [
    ("flac/prefix", StreamKind::Unknown),
    ("flac/layout", StreamKind::Unknown),
    ("flac/junk", StreamKind::Unknown),
    ("flac/headers", StreamKind::Unknown),
    ("flac/subframes", StreamKind::Unknown),
    ("flac/warmup", StreamKind::Unknown),
    ("flac/rans", StreamKind::Compressed),
    ("flac/raw0", StreamKind::Compressed),
    ("flac/raw1", StreamKind::Compressed),
];

/// The formula of `input`, a FLAC file, on `threads` threads (the formula does not depend
/// on it). None where the input is not FLAC, or the coder panics.
pub fn encode(input: &[u8], threads: usize) -> Option<Formula> {
    let c = std::panic::catch_unwind(|| codec::encode_threads(input, threads)).ok()??;
    let mut f = Formula::default();
    let words = c.words.iter().flat_map(|w| w.to_le_bytes()).collect();
    let [raw0, raw1] = c.raw;
    let slots = c.streams.into_iter().chain([words, raw0, raw1]);
    let inputs = slots.zip(CLASSES).map(|(s, (class, kind))| f.push_as(class, kind, s)).collect();
    f.root = Node::apply_with(MODULE, vec![resource()], Vec::new(), inputs, input.len() as u64);
    Some(f)
}

/// The module's resource: the built-in frequency tables of aqr-raw's residual model, which
/// its `pcm-recons` shares.
pub fn resource() -> Resource {
    aqr_raw::formula::pcm::resource()
}

/// The module: the file of `len` bytes from `resources` (the tables) and `inputs` (the
/// streams).
pub fn decode(params: &[u8], resources: &[&[u8]], inputs: &[&[u8]], len: u64, threads: usize) -> Result<Vec<u8>> {
    let corrupt = |s: &str| Error(format!("corrupt: {s}"));
    if !params.is_empty() {
        return Err(corrupt("params"));
    }
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
    codec::decode_threads(&c, &t, len as usize, &mut out, threads).map_err(|e| corrupt(e.0))?;
    if out.len() as u64 != len {
        return Err(corrupt("length"));
    }
    Ok(out)
}
