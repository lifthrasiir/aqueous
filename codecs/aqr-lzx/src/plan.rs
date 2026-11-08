//! The memory plan of the module `lzx-recons` on WebAssembly ([`aqr_api::stack`]), as
//! [`crate::module::decode`] lays it out: the instance's memory taken block by block in the order
//! the module takes it ([`Sim`]). The module spawns no tasks. What it takes follows from the
//! window, the streams' lengths, their records' starts (the capacities of a block's tokens and
//! frames) and the length of its output.

use aqr_api::stack::{Sim, block, whole};
use aqr_common::arith::ArithDecoder;

use crate::module::{cut_lens, spare_data, variant};
use crate::recons::{CTX_VECS, SCRATCH, read_caps, wasm};

/// The memory plan of `lzx-recons` ([`crate::module::decode`]): of its initial memory (`initial`
/// bytes), parameters `params`, inputs of `inputs` bytes (data, records, bytes kept, index), an
/// output of `len`, and the contents of the records and the index. The data and the bytes kept
/// are asked for their lengths only: they may not be at hand (sunk) when a plan is made.
pub fn decode(initial: u64, params: &[u8], inputs: &[u64], len: u64, [records, index]: [&[u8]; 2]) -> Option<u64> {
    let &[data, rec, kept, _] = inputs else { return None };
    if records.len() as u64 != rec {
        return None;
    }
    let v = variant(params).ok()?;
    let size = |n: u64| usize::try_from(n).ok();
    let (cuts, _) = cut_lens(index, [size(kept)?, size(data)?, size(rec)?]).ok()?;
    // (each stream's data length and record)
    let mut rest = records;
    let streams: Vec<(usize, &[u8])> = cuts
        .iter()
        .map(|&[_, d, r]| {
            let (record, after) = rest.split_at(r);
            rest = after;
            (d, record)
        })
        .collect();
    let mut m = Sim::new(initial);
    m.take(whole(params.len() as u64, &[], inputs));
    m.take(wasm::STREAM * streams.len() as u64);
    m.take(len);
    // (the match finder and the chunk's data, made once for the streams)
    let window = v.window() as u64;
    for n in [65536, window, 65536, window] {
        m.take(4 * n);
    }
    m.take(spare_data(v, streams.iter().map(|s| s.0)) as u64);
    for &(data, record) in &streams {
        let top = m.top;
        let caps = read_caps(&mut ArithDecoder::new(record), data).ok()?;
        // (`recons::join_in`: the walker's contexts, its code lengths and the writer's, the
        // block's tokens and frames' padding, then the work of the blocks' trees)
        for (n, k) in CTX_VECS {
            m.take(wasm::BIT * (n * k) as u64);
        }
        m.take(v.main() as u64);
        m.take(v.main() as u64);
        m.take(wasm::TOK * caps.toks as u64);
        m.take(2 * caps.pads as u64);
        m.peak(block(SCRATCH as u64));
        m.top = top;
    }
    Some(m.plan())
}
