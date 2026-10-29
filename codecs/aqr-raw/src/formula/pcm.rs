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

pub const MODULE: Module = Module::new("pcm-recons", 1).with_memory(memory);

/// The memory of the module: [`pipeline_memory`] of frames of 4096 samples a channel, of the
/// samples the raw bytes are.
fn memory(u: &aqr_formula::Usage<'_>) -> Option<u64> {
    let fmt = Format::from_bytes(u.params)?;
    let &[.., words, raw0, raw1] = u.inputs else { return None };
    let samples = u.output() / fmt.frame_bytes() as u64 * fmt.channels as u64;
    Some(pipeline_memory(u, (pcm::BLOCK * fmt.channels) as u64, samples, words, raw0 + raw1))
}

/// The memory of a pipeline of `audio::par::apart` (pcm-recons' and flac-recons'), of frames of
/// `frame` samples at most (all channels) and `samples` in all at most: its 8 tasks all running,
/// each a copy of the memory as it spawns them ([`aqr_formula::Usage::base`]) and the batches it
/// holds (the message it got, the batch decoded and the message it makes, growing: some 44 bytes
/// a sample, a batch of `BATCH_SAMPLES` and a frame more), the error stage's rANS `words` and
/// padded `raw` bits; the messages its channels queue (24, 8 bytes a sample, no more than the
/// samples through each of the 4 stages); and the output, reserved after the spawn.
pub fn pipeline_memory(u: &aqr_formula::Usage<'_>, frame: u64, samples: u64, words: u64, raw: u64) -> u64 {
    let batch = (crate::audio::par::BATCH_SAMPLES as u64 + frame).min(samples);
    let tasks = 8;
    let held = 44 * batch + (64 << 10);
    let queued = 8 * (24 * batch).min(4 * samples);
    (1 + tasks) * u.base() + tasks * held + words + raw + 2 * 64 + queued + u.output() + 4 * aqr_formula::PAGE
}

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
