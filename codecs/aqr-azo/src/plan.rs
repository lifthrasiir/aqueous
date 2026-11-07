//! The memory plan of the module `azo-recons` on WebAssembly ([`aqr_api::stack`]), as
//! [`crate::module::decode`] lays it out: the instance's memory taken block by block in the order
//! the module takes it ([`Sim`]). The module spawns no tasks. What it takes follows from its
//! inputs (the streams' records say how many blocks a stream has and how long the coded ones
//! are) and the length of its output.

use aqr_api::stack::{Sim, block, whole};

use crate::azo::MODEL_BYTES;
use crate::recons::{CTX_LENS, KEYS, PAIRS, VIABLE, block_sizes, wasm};

/// Bytes of what rebuilding a coded block of `n` bytes takes, as `recons::code_onto` takes it:
/// the models, then what `recons::walk` makes: the match finder's heads, chains and room for the
/// pairs found, the record's contexts, the pairs of this position and the next, and the
/// remembered matches that fit. The code is written onto the output.
fn coded(n: u64) -> u64 {
    let models: u64 = MODEL_BYTES.iter().map(|&b| block(b)).sum();
    let finder =
        block((4 * KEYS.len() as u64) << 16) + block(4 * KEYS.len() as u64 * n) + block(wasm::PAIR * PAIRS as u64);
    let contexts: u64 = CTX_LENS.iter().map(|&l| block(4 * l as u64)).sum();
    let pairs = 2 * block(wasm::PAIR * PAIRS as u64) + block(wasm::WORD * VIABLE as u64);
    models + finder + contexts + pairs
}

/// The memory plan of `azo-recons` ([`crate::module::decode`]): of its initial memory (`initial`
/// bytes), parameters `params`, inputs of `inputs` bytes, an output of `len`, and the inputs
/// themselves.
pub fn decode(initial: u64, params: &[u8], inputs: &[u64], len: u64, files: [&[u8]; 3]) -> Option<u64> {
    let [data, records, index] = files;
    let streams = aqr_common::streams::cut(data, records, index, || ()).ok()?;
    let mut m = Sim::new(initial);
    m.take(whole(params.len() as u64, &[], inputs));
    m.take(wasm::STREAM * streams.len() as u64);
    m.take(len);
    for (data, record) in streams {
        let (blocks, sizes) = block_sizes(data, record).ok()?;
        // (the stream's blocks, held while they are written)
        let top = m.top;
        m.take(wasm::BLOCK * blocks as u64);
        for n in sizes {
            m.peak(coded(n as u64));
        }
        m.top = top;
    }
    Some(m.plan())
}
