//! The memory plans of the modules `lzma-recons` and `lzma2-recons` on WebAssembly
//! ([`aqr_api::stack`]), as [`crate::module::decode_part`] lays them out: the instance's memory
//! taken block by block in the order the modules take it ([`Sim`]). The modules are part
//! modules, spawn no tasks and hold nothing of the lengths of their inputs and output but what a
//! stream's dictionary asks for: the window of its data (the dictionary and some slack, or the
//! stream where it is shorter), the match finder's rings and tables, and the walk's contexts.
//! What the plan reads of the inputs is each stream's length and its record's start (the
//! dictionary size and, for LZMA, the properties).

use aqr_api::stack::Sim;

use crate::lzma::lit_len;
use crate::lzma2::{CHUNK_ROOM, LIT_MAX};
use crate::mf;
use crate::module::INDEX_PIECE;
use crate::recons::{self, CTX_BITS, PAIRS, wasm::BIT, wasm::PAIR};
use crate::stream::PIECE;
use crate::window;

/// The bytes of data of each stream, and its record.
type Streams<'a> = Vec<(usize, &'a [u8])>;

/// What is taken before the streams: the instance's memory, and the pieces of the readers of the
/// index and the records. The streams ([`aqr_common::streams::cut`]) are those of the inputs,
/// the lengths of their data and their records.
fn start(initial: u64, files: [&[u8]; 3]) -> Option<(Sim, Streams<'_>)> {
    let [data, records, index] = files;
    let streams = aqr_common::streams::cut(data, records, index, || ()).ok()?;
    let mut m = Sim::new(initial);
    m.take(INDEX_PIECE as u64);
    m.take(PIECE as u64);
    Some((m, streams.into_iter().map(|(d, r)| (d.len(), r)).collect()))
}

/// What a stream takes over its start, as [`recons::join_stream`] and [`crate::lzma2::join_stream`]
/// lay it out: the window of its data, the match finder, (the encoder's) and then the walk's
/// contexts and pairs.
fn stream(m: &mut Sim, n: usize, dict: u32, own: impl FnOnce(&mut Sim)) {
    let reach = recons::reach(dict);
    m.take(window::room(n, reach) as u64);
    for b in mf::blocks(n, reach) {
        m.take(b);
    }
    own(m);
    m.take(BIT * CTX_BITS as u64);
    m.take(PAIR * PAIRS as u64);
}

/// The memory plan of `lzma-recons` ([`crate::module::decode_part`]): of its initial memory
/// (`initial` bytes) and its inputs themselves (the data, the records and the index).
pub fn decode(initial: u64, files: [&[u8]; 3]) -> Option<u64> {
    let (mut m, streams) = start(initial, files)?;
    for (n, record) in streams {
        let h = recons::head_of(record, n)?;
        let top = m.top;
        // (`StreamEncoder::to`: the coder's piece, then the model's literal probabilities)
        stream(&mut m, n, h.dict, |m| {
            m.take(PIECE as u64);
            m.take(2 * lit_len(h.props) as u64);
        });
        m.top = top;
    }
    Some(m.plan())
}

/// The memory plan of `lzma2-recons` ([`crate::module::decode2_part`]), as [`decode`] for
/// segments.
pub fn decode2(initial: u64, files: [&[u8]; 3]) -> Option<u64> {
    let (mut m, streams) = start(initial, files)?;
    for (n, record) in streams {
        let h = crate::lzma2::head_of(record)?;
        let top = m.top;
        // (`Writer::to`: the model's literal probabilities, for the largest properties; and an
        // LZMA chunk's coder on top of it all, where there is a chunk)
        stream(&mut m, n, h.dict, |m| m.take(2 * LIT_MAX as u64));
        if n > 0 {
            m.peak(CHUNK_ROOM as u64);
        }
        m.top = top;
    }
    Some(m.plan())
}
