//! aqr-mp3 as a module of entry formulas (see aqr-formula). The formula is one application of
//! [`MODULE`] on ten input slots, the streams of the coded form ([`crate::codec::Coded`])
//! stored raw, and no parameters. FORMAT.md specifies it.

use crate::codec::{self, Coded, N_BYTE_STREAMS};
use crate::par;
use aqr_formula::{Formula, Kind, Module, Node, Resource};
use std::sync::OnceLock;

pub const MODULE: Module = Module { name: "mp3-recons", version: 2 };

#[derive(Debug)]
pub struct Error(pub String);

/// Slots: the eight byte streams of the coded form, then the rANS words (u32 LE) and the raw
/// bits.
const SLOTS: usize = N_BYTE_STREAMS + 2;

/// Classes of the slots, and their kinds: the side information, the rANS words and the raw
/// bits do not compress.
const CLASSES: [(&str, Kind); SLOTS] = [
    ("mp3/segs", Kind::Unknown),
    ("mp3/junk", Kind::Unknown),
    ("mp3/headers", Kind::Unknown),
    ("mp3/crc", Kind::Unknown),
    ("mp3/tailbits", Kind::Unknown),
    ("mp3/rawgr", Kind::Unknown),
    ("mp3/gaps", Kind::Unknown),
    ("mp3/side", Kind::Compressed),
    ("mp3/rans", Kind::Compressed),
    ("mp3/rawbits", Kind::Compressed),
];

/// The formula of `input`: `None` where the coded form is not smaller (or on a failure).
/// `threads` threads give the same formula.
pub fn encode(input: &[u8], threads: usize) -> Option<Formula> {
    let c = std::panic::catch_unwind(|| {
        let c = if threads > 1 && input.len() >= par::PAR_MIN { par::encode(input) } else { codec::encode(input) };
        let n: usize = c.streams.iter().map(Vec::len).sum::<usize>() + c.words.len() * 4 + c.raw.len();
        (n + 64 < input.len()).then_some(c)
    })
    .ok()??;
    let mut f = Formula::default();
    let mut slots: Vec<Vec<u8>> = c.streams;
    slots.push(c.words.iter().flat_map(|w| w.to_le_bytes()).collect());
    slots.push(c.raw);
    let inputs = slots.into_iter().zip(CLASSES).map(|(d, (class, kind))| f.push_as(class, kind, d)).collect();
    f.root = Node::apply_with(MODULE, vec![resource()], Vec::new(), inputs, input.len() as u64);
    Some(f)
}

/// The module's resource: the built-in frequency tables, uncompressed.
pub fn resource() -> Resource {
    static R: OnceLock<Resource> = OnceLock::new();
    R.get_or_init(|| Resource::new(codec::builtin_raw())).clone()
}

/// The module: the file of `len` bytes rebuilt from the ten `inputs` and the `resources` (the
/// tables), with `threads` threads (the same output whatever the number).
pub fn decode(
    params: &[u8],
    resources: &[&[u8]],
    inputs: &[&[u8]],
    len: u64,
    threads: usize,
) -> Result<Vec<u8>, Error> {
    let corrupt = |m: &str| Error(format!("corrupt: {m}"));
    if !params.is_empty() {
        return Err(corrupt("params"));
    }
    let [res] = resources else { return Err(corrupt("resources")) };
    let t = codec::tables_from(res).ok_or_else(|| corrupt("tables"))?;
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
        codec::decode(&c, &t).map_err(|e| corrupt(e.0))?
    } else {
        let streams: [&[u8]; N_BYTE_STREAMS] = std::array::from_fn(|i| inputs[i]);
        par::decode(&t, len, &streams, words, raw, threads).map_err(|e| corrupt(e.0))?
    };
    if out.len() as u64 != len {
        return Err(corrupt("length"));
    }
    Ok(out)
}
