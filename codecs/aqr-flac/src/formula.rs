//! aqr-flac as a module of entry formulas (see aqr-formula): the coded streams as input
//! slots.
//!
//! No params. Resources: aqr-raw's residual tables. Inputs: the six byte streams
//! (`codec::STREAM_NAMES`), the rANS words (u32 LE) and the two raw bit streams. FORMAT.md
//! describes the module in full. WAV, AIFF and headerless PCM are aqr-raw's (`pcm-recons`).

use crate::codec::{self, N_BYTE_STREAMS};
use aqr_formula::{Formula, Kind as StreamKind, Module, Node, Resource};
use std::borrow::Cow;

pub use crate::module::{Error, Result, decode};

pub const MODULE: Module = Module::new("flac-recons", 3).with_memory(memory);

/// The memory of the module: aqr-raw's `pipeline_memory` of frames of the stream's largest
/// block (STREAMINFO's maximum block size) of all its channels.
fn memory(u: &aqr_formula::Usage<'_>) -> Option<u64> {
    let &[.., words, raw0, raw1] = u.inputs else { return None };
    let prefix = u.contents.get(codec::S_PREFIX)?;
    let (si, _) = crate::flac::parse_metadata(prefix)?;
    // (STREAMINFO is the first block: its maximum block size 2 bytes in)
    let at = prefix.windows(4).position(|w| w == b"fLaC")? + 8;
    let max = u16::from_be_bytes(prefix.get(at + 2..at + 4)?.try_into().ok()?) as u64;
    Some(aqr_raw::formula::pcm::pipeline_memory(u, max * si.channels as u64, u64::MAX, words, raw0 + raw1))
}

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
    let [raw0, raw1] = c.raw;
    let slots = c.streams.into_iter().chain([c.words, raw0, raw1]).map(Cow::into_owned);
    let inputs = slots.zip(CLASSES).map(|(s, (class, kind))| f.push_as(class, kind, s)).collect();
    f.root = Node::apply_with(MODULE, vec![resource()], Vec::new(), inputs, input.len() as u64);
    Some(f)
}

/// The module's resource: the built-in frequency tables of aqr-raw's residual model, which
/// its `pcm-recons` shares.
pub fn resource() -> Resource {
    aqr_raw::formula::pcm::resource()
}
