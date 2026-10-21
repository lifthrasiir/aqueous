//! aqr-mp3 as a module of entry formulas (see aqr-formula): the coded form without the
//! container around it. The formula is one application of [`MODULE`] on ten input slots, the
//! streams of the container ([`crate::codec::Coded`]) stored raw, and no parameters.
//! FORMAT.md, "Formula module", specifies it.

use crate::codec::{self, Coded, N_BYTE_STREAMS};
use crate::container::{Error, pack};
use crate::par;
use aqr_common::varint;
use aqr_formula::{Formula, Module, Node};

pub const MODULE: Module = Module { name: "mp3-recons", version: 1 };

/// Slots: the eight byte streams of the container, then the rANS words (u32 LE) and the raw
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

/// The length the container would have, as [`crate::container::compress`] computes it.
fn container_len(c: &Coded) -> usize {
    let mut n = 9 + 16;
    let mut v = Vec::new();
    for b in &c.streams {
        v.clear();
        match pack(b) {
            Some(z) => {
                varint::put(&mut v, (z.len() as u64) << 1 | 1);
                varint::put(&mut v, b.len() as u64);
                n += v.len() + z.len();
            }
            None => {
                varint::put(&mut v, (b.len() as u64) << 1);
                n += v.len() + b.len();
            }
        }
    }
    for l in c.bit_lens {
        varint::put(&mut v, l);
    }
    varint::put(&mut v, c.words.len() as u64);
    varint::put(&mut v, c.raw_bits);
    n + v.len() + c.words.len() * 4 + c.raw.len()
}

/// The formula of `input`: `None` where the container would store it (or on a failure).
/// `threads` threads give the same formula.
pub fn encode(input: &[u8], threads: usize) -> Option<Formula> {
    let c = std::panic::catch_unwind(|| {
        let c = if threads > 1 && input.len() >= par::PAR_MIN {
            par::encode(input, |_| None).0
        } else {
            codec::encode(input)
        };
        // Streams stored raw already below the input leave no doubt; else measure.
        let raw: usize = c.streams.iter().map(Vec::len).sum::<usize>() + c.words.len() * 4 + c.raw.len();
        (raw + 64 < input.len() || container_len(&c) < input.len() + 9).then_some(c)
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
        let srcs: [par::Src; N_BYTE_STREAMS] = std::array::from_fn(|i| par::Src::Plain(inputs[i]));
        par::decode(len, &srcs, words, raw, threads).map_err(|e| match e {
            par::DecError::Stream(m) => Error(m),
            par::DecError::Codec(e) => corrupt(e.0),
        })?
    };
    if out.len() as u64 != len {
        return Err(corrupt("length"));
    }
    Ok(out)
}
