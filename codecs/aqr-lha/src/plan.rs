//! The memory plan of the module `lha-recons` on WebAssembly ([`aqr_api::stack`]), as
//! [`crate::module::decode`] lays it out: the instance's memory taken block by block in the order
//! the module takes it ([`Sim`]). The module spawns no tasks. What it takes follows from the
//! streams' lengths, their records' starts (the reach and the count of tokens) and the length of
//! its output.

use aqr_api::stack::{Sim, block, whole};

use crate::recons::{CTX_LENS, KEYS, PAIRS, SCRATCH, ring_slots, start_of, tree_slots, wasm};

/// The memory plan of `lha-recons` ([`crate::module::decode`]): of its initial memory (`initial`
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
        let n = data.len();
        let s = start_of(record, n).ok()?;
        let top = m.top;
        // (`recons::join_onto`: the contexts, the tokens, then the walk's match finders and pairs,
        // then the headers' work)
        for l in CTX_LENS {
            m.take(4 * l as u64);
        }
        m.take(wasm::TOKEN * s.tokens as u64);
        let slots = ring_slots(n) as u64;
        let tree = 4 * (tree_slots(n, s.reach) as u64 + 257);
        let pairs = block(wasm::PAIR * PAIRS as u64);
        m.peak(
            block((4 * KEYS.len() as u64) << 16)
                + block(4 * KEYS.len() as u64 * slots)
                + pairs
                + 3 * block(tree)
                + 2 * pairs,
        );
        m.peak(block(SCRATCH as u64));
        m.top = top;
    }
    Some(m.plan())
}
