//! The memory plan of the module `xpress-recons` on WebAssembly ([`aqr_api::stack`]), as
//! [`crate::module::decode`] lays it out: the instance's memory taken block by block in the order
//! the module takes it ([`Sim`]). The module spawns no tasks. What it takes follows from the
//! streams' lengths, their records' starts (the chunk size, which decides the tokens' capacity)
//! and the length of its output.

use aqr_api::stack::{Sim, block, whole};
use aqr_common::arith::ArithDecoder;

use crate::module::cut_lens;
use crate::recons::{CTX_VECS, MF_BYTES, SCRATCH, SLACK, block_toks, read_head, wasm};

/// The memory plan of `xpress-recons` ([`crate::module::decode`]): of its initial memory
/// (`initial` bytes), parameters `params`, inputs of `inputs` bytes (data, records, bytes kept,
/// index), an output of `len`, and the contents of the records and the index. The data and the
/// bytes kept are asked for their lengths only: they may not be at hand (sunk) when a plan is
/// made.
pub fn decode(initial: u64, params: &[u8], inputs: &[u64], len: u64, [records, index]: [&[u8]; 2]) -> Option<u64> {
    let &[data, rec, kept, _] = inputs else { return None };
    if records.len() as u64 != rec || !params.is_empty() {
        return None;
    }
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
    m.take(len + SLACK as u64);
    // (the match finder, made once for the streams)
    m.take(MF_BYTES);
    m.take(MF_BYTES);
    for &(data, record) in &streams {
        if data == 0 {
            continue;
        }
        let top = m.top;
        let (chunk, _) = read_head(&mut ArithDecoder::new(record), data).ok()?;
        // (`recons::join_in`: the walker's contexts and the block's tokens, then the work of the
        // blocks' tables)
        for (n, k) in CTX_VECS {
            m.take(wasm::BIT * (n * k) as u64);
        }
        m.take(wasm::TOK * block_toks(chunk) as u64);
        m.peak(block(SCRATCH as u64));
        m.top = top;
    }
    Some(m.plan())
}
