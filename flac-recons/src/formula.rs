//! flac-recons as a module of entry formulas (see recons-formula): the container's streams as
//! input slots instead of one file, without its zstd, length and checksum.
//!
//! Params: one byte, 0 for FLAC and 1 for PCM (WAV, AIFF and raw samples decode alike).
//! Inputs: the six byte streams (`codec::STREAM_NAMES`), the rANS words (u32 LE) and the two
//! raw bit streams. FORMAT.md describes the module in full.

use crate::codec::{Coded, N_BYTE_STREAMS};
use crate::container::{self, Error, Kind, Options};
use recons_formula::{Formula, Module, Node};

pub const MODULE: Module = Module { name: "flac-recons", version: 1 };

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

/// [`encode_with`] with the default options (kind detected) and `threads`.
pub fn encode(input: &[u8], threads: usize) -> Option<Formula> {
    encode_with(input, &Options { threads, ..Options::default() })
}

/// The formula of `input` (FLAC, WAV / AIFF, or headerless PCM if `opt.raw` is given). None
/// where the input is not coded (the container would store it), or the coder panics.
pub fn encode_with(input: &[u8], opt: &Options) -> Option<Formula> {
    let (kind, c) = std::panic::catch_unwind(|| {
        let kind = Kind::of(input, opt);
        container::encode(input, opt, kind).map(|c| (kind, c))
    })
    .ok()??;
    let mut f = Formula::default();
    let words = c.words.iter().flat_map(|w| w.to_le_bytes()).collect();
    let [raw0, raw1] = c.raw;
    let slots = c.streams.into_iter().chain([words, raw0, raw1]);
    let inputs = slots.zip(CLASSES).map(|(s, class)| f.push(class, s)).collect();
    f.root = Node::apply(MODULE, vec![(kind != Kind::Flac) as u8], inputs, input.len() as u64);
    Some(f)
}

/// The module: the output of `len` bytes from `params` (the kind) and `inputs` (the streams).
pub fn decode(params: &[u8], inputs: &[&[u8]], len: u64, threads: usize) -> Result<Vec<u8>> {
    let corrupt = |s: &str| Error(format!("corrupt: {s}"));
    let kind = match params {
        [0] => Kind::Flac,
        [1] => Kind::Pcm,
        _ => return Err(corrupt("params")),
    };
    let [streams @ .., words, raw0, raw1] = inputs else { return Err(corrupt("inputs")) };
    if streams.len() != N_BYTE_STREAMS || words.len() % 4 != 0 {
        return Err(corrupt("inputs"));
    }
    // The same bound as the container's: no output is larger than 64 times its coded form.
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
    container::decode(kind, &c, len as usize, &mut out, threads).map_err(|e| corrupt(e.0))?;
    if out.len() as u64 != len {
        return Err(corrupt("length"));
    }
    Ok(out)
}
