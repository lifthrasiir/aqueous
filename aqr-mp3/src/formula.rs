//! aqr-mp3 as a module of entry formulas (see aqr-formula). The formula is one application of
//! [`MODULE`] on ten input slots, the streams of the coded form ([`crate::codec::Coded`])
//! stored raw, and no parameters. FORMAT.md specifies it.

use crate::codec::{self, Coded, N_BYTE_STREAMS};
use crate::par;
use aqr_formula::{Formula, Module, Node};

pub const MODULE: Module = Module { name: "mp3-recons", version: 1 };

#[derive(Debug)]
pub struct Error(pub String);

/// Slots: the eight byte streams of the coded form, then the rANS words (u32 LE) and the raw
/// bits.
const SLOTS: usize = N_BYTE_STREAMS + 2;

/// Classes of the slots.
const CLASSES: [&str; SLOTS] = [
    "mp3/segs",
    "mp3/junk",
    "mp3/headers",
    "mp3/crc",
    "mp3/tailbits",
    "mp3/rawgr",
    "mp3/gaps",
    "mp3/side",
    "mp3/rans",
    "mp3/rawbits",
];

/// The formula of `input`: `None` where the coded form is not smaller (or on a failure).
/// `threads` threads give the same formula.
pub fn encode(input: &[u8], threads: usize) -> Option<Formula> {
    let c = std::panic::catch_unwind(|| {
        let c = if threads > 1 && input.len() >= par::PAR_MIN {
            par::encode(input)
        } else {
            codec::encode(input)
        };
        let n: usize = c.streams.iter().map(Vec::len).sum::<usize>() + c.words.len() * 4 + c.raw.len();
        (n + 64 < input.len()).then_some(c)
    })
    .ok()??;
    let mut f = Formula::default();
    let mut slots: Vec<Vec<u8>> = c.streams;
    slots.push(c.words.iter().flat_map(|w| w.to_le_bytes()).collect());
    slots.push(c.raw);
    let inputs = slots.into_iter().zip(CLASSES).map(|(d, class)| f.push(class, d)).collect();
    f.root = Node::apply(MODULE, Vec::new(), inputs, input.len() as u64);
    Some(f)
}

/// The module: the file of `len` bytes rebuilt from the ten `inputs`, with `threads` threads
/// (the same output whatever the number).
pub fn decode(params: &[u8], inputs: &[&[u8]], len: u64, threads: usize) -> Result<Vec<u8>, Error> {
    let corrupt = |m: &str| Error(format!("corrupt: {m}"));
    if !params.is_empty() {
        return Err(corrupt("params"));
    }
    let &[.., words, raw] = inputs else { return Err(corrupt("inputs")) };
    if inputs.len() != SLOTS || words.len() % 4 != 0 {
        return Err(corrupt("inputs"));
    }
    let total: usize = inputs.iter().map(|s| s.len()).sum();
    let out = if threads <= 1 || total < par::PAR_MIN {
        let c = Coded {
            len,
            streams: inputs[..N_BYTE_STREAMS].iter().map(|s| s.to_vec()).collect(),
            words: words.chunks_exact(4).map(|w| u32::from_le_bytes(w.try_into().unwrap())).collect(),
            raw: raw.to_vec(),
            raw_bits: raw.len() as u64 * 8,
            bit_lens: [0; 3],
        };
        codec::decode(&c).map_err(|e| corrupt(e.0))?
    } else {
        let streams: [&[u8]; N_BYTE_STREAMS] = std::array::from_fn(|i| inputs[i]);
        par::decode(len, &streams, words, raw, threads).map_err(|e| corrupt(e.0))?
    };
    if out.len() as u64 != len {
        return Err(corrupt("length"));
    }
    Ok(out)
}
