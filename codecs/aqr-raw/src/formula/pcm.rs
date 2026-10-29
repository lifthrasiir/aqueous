//! The module `pcm-recons`: interleaved PCM samples from their coded streams
//! ([`crate::audio::pcm`]).
//!
//! Params: the sample format (3 bytes, [`Format::to_bytes`]). Resources: the residual model's
//! frequency tables. Inputs: the three byte streams (channel assignments, subframe
//! parameters, warm-up samples), the rANS words (u32 LE) and the two raw bit streams.

use crate::audio::pcm::{self, Format, N_BYTE_STREAMS};
use crate::audio::residual;
use aqr_formula::{Formula, Kind, Module, Node, Resource};
use std::borrow::Cow;
use std::sync::OnceLock;

pub use crate::module::pcm::decode;

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
    let [raw0, raw1] = c.raw;
    let slots = c.streams.into_iter().chain([c.words, raw0, raw1]).map(Cow::into_owned);
    let inputs = slots.zip(CLASSES).map(|(s, (class, kind))| f.push_as(class, kind, s)).collect();
    Some(Node::apply_with(MODULE, vec![resource()], fmt.to_bytes().to_vec(), inputs, samples.len() as u64))
}
