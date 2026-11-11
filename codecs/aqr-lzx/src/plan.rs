//! The memory plan of the module `lzx-recons` on WebAssembly ([`aqr_api::stack`]), as
//! [`crate::module::decode_part`] lays it out: the instance's memory taken block by block in the
//! order the module takes it ([`Sim`]). The module is a part module and spawns no tasks. What it
//! takes follows from the window, the records' starts (the capacities of a block's tokens and
//! frames, and the bytes a block covers) and the streams' lengths, but not from the lengths of
//! its output: of a CAB stream it holds the window and the block being coded, as many bytes as
//! frames hold ([`recons::held`]).

use aqr_api::stack::{Sim, block};
use aqr_common::arith::ArithDecoder;
use aqr_lzma::stream::PIECE;

use crate::lzx;
use crate::module::{INDEX_PIECE, cut_lens, variant};
use crate::recons::{self, CTX_VECS, SCRATCH, read_caps, wasm};

/// The memory plan of `lzx-recons` ([`crate::module::decode_part`]): of its initial memory
/// (`initial` bytes), parameters `params`, inputs of `inputs` bytes (data, records, bytes kept,
/// index), and the contents of the records and the index. The data and the bytes kept are asked
/// for their lengths only: they may not be at hand (sunk) when a plan is made.
pub fn decode(initial: u64, params: &[u8], inputs: &[u64], [records, index]: [&[u8]; 2]) -> Option<u64> {
    let &[data, rec, kept, _] = inputs else { return None };
    if records.len() as u64 != rec {
        return None;
    }
    let v = variant(params).ok()?;
    let size = |n: u64| usize::try_from(n).ok();
    let (cuts, _) = cut_lens(index, [size(kept)?, size(data)?, size(rec)?]).ok()?;
    let mut m = Sim::new(initial);
    // (the readers of the index, the records and the bytes kept)
    m.take(INDEX_PIECE as u64);
    m.take(PIECE as u64);
    m.take(PIECE as u64);
    // (the match finder, made once for the streams)
    let window = v.window() as u64;
    for n in [65536, window, 65536, window] {
        m.take(4 * n);
    }
    let mut rest = records;
    for &[_, data, r] in &cuts {
        let top = m.top;
        let (record, after) = rest.split_at(r);
        rest = after;
        let caps = read_caps(&mut ArithDecoder::new(record), data).ok()?;
        // (`recons::join_stream`: the walker's contexts, its code lengths and the writer's, the
        // block's tokens and frames' padding, then the work of the blocks' trees)
        for (n, k) in CTX_VECS {
            m.take(wasm::BIT * (n * k) as u64);
        }
        m.take(v.main() as u64);
        m.take(v.main() as u64);
        m.take(wasm::TOK * caps.toks as u64);
        m.take(2 * caps.pads as u64);
        if v.cab {
            // (the data held, and the writer's output)
            m.take(recons::held(v, caps.block, data));
            m.take(2 * lzx::FLUSH as u64);
        } else if data > 0 {
            // (WIM's: the chunk, its copy, and for each chunk the writer's output)
            let chunk = recons::CHUNK.min(data);
            m.take(chunk as u64);
            m.take(chunk as u64);
            m.take(2 * lzx::FLUSH as u64);
        }
        m.peak(block(SCRATCH as u64));
        m.top = top;
    }
    Some(m.plan())
}
