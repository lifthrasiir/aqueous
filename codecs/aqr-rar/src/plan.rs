//! The memory plans of the modules `rar5-recons` and `rar3-recons` on WebAssembly
//! ([`aqr_api::stack`]), as [`crate::module`] lays them out: the instance's memory taken block by
//! block in the order the modules take it ([`Sim`]). The modules spawn no tasks. What they take
//! follows from the streams' lengths, their records' starts (the counts of what they hold) and
//! the length of their output.

use aqr_api::stack::{Sim, block, whole};

use crate::recons5::{self, CTX_LENS, SCRATCH, wasm};
use crate::{lz5, rec::Mf};

/// The memory plan of `rar5-recons` ([`crate::module::decode5`]): of its initial memory
/// (`initial` bytes), parameters `params`, inputs of `inputs` bytes, an output of `len`, and the
/// inputs themselves.
pub fn decode5(initial: u64, params: &[u8], inputs: &[u64], len: u64, files: [&[u8]; 3]) -> Option<u64> {
    let [data, records, index] = files;
    let streams = aqr_common::streams::cut(data, records, index, || ()).ok()?;
    let mut m = Sim::new(initial);
    m.take(whole(params.len() as u64, &[], inputs));
    m.take(wasm::STREAM * streams.len() as u64);
    m.take(len);
    for (data, record) in streams {
        let n = data.len();
        let s = recons5::start_of(record, n).ok()?;
        let top = m.top;
        let files = s.sizes.len() as u64;
        // (`recons5::join_onto`: the contexts, the files' sizes, the unfiltered data, the filters
        // and where each file's end, the tokens and where each file's end, the blocks and where
        // each file's end, the codes written, then the headers' work)
        for l in CTX_LENS {
            m.take(wasm::BIT * l as u64);
        }
        m.take(4 * files);
        m.take(n as u64);
        m.take(wasm::FILTER * s.filters as u64);
        m.take(4 * files);
        // (the delta filters' copy of their bytes)
        m.peak(block(s.delta as u64));
        m.take(wasm::TOKEN * (s.tokens + s.filters) as u64);
        m.take(4 * files);
        let (heads, rings) = Mf::slots(n);
        let pairs = block(wasm::PAIR * Mf::PAIRS as u64);
        // (the walk's match finder, then the pairs of two positions)
        m.peak(block(4 * heads as u64) + block(4 * rings as u64) + pairs + 2 * pairs);
        m.take(wasm::BLOCK * s.blocks as u64);
        m.take(4 * files);
        for l in [lz5::MAIN, lz5::dists(s.v1), lz5::LOW, lz5::REPLEN] {
            m.take(wasm::CODE * l as u64);
        }
        m.peak(block(SCRATCH as u64));
        m.top = top;
    }
    Some(m.plan(1))
}
