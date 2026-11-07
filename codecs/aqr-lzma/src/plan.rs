//! The memory plans of the modules `lzma-recons` and `lzma2-recons` on WebAssembly
//! ([`aqr_api::stack`]), as [`crate::module::decode`] lays them out: the instance's memory taken
//! block by block in the order the modules take it ([`Sim`]). The modules spawn no tasks. What
//! they take follows from the streams' lengths and their records' starts (the properties and
//! dictionary sizes, the parts or the chunks), and the length of the output, which they write the
//! streams onto, each at its end.

use aqr_api::stack::{Sim, whole};

use crate::lzma::lit_len;
use crate::lzma2::{self, CHUNK_ROOM, LIT_MAX};
use crate::mf;
use crate::recons::{self, CTX_BITS, PAIRS, wasm::BIT, wasm::PAIR, wasm::PART};

/// What is taken before the streams: the instance's memory and the copies [`whole`] makes, the
/// list of the streams and the output.
fn start<'a>(
    initial: u64,
    params: &[u8],
    inputs: &[u64],
    len: u64,
    files: [&'a [u8]; 3],
) -> Option<(Sim, Vec<(usize, &'a [u8])>)> {
    let [data, records, index] = files;
    let streams = aqr_common::streams::cut(data, records, index, || ()).ok()?;
    let mut m = Sim::new(initial);
    m.take(whole(params.len() as u64, &[], inputs));
    m.take(16 * streams.len() as u64);
    m.take(len);
    Some((m, streams.into_iter().map(|(d, r)| (d.len(), r)).collect()))
}

/// What a walk of the record takes over a match finder: the contexts and the pairs.
fn walk(m: &mut Sim, n: usize, dict: u32) {
    for b in mf::blocks(n, recons::reach(dict)) {
        m.take(b);
    }
    m.take(BIT * CTX_BITS as u64);
    m.take(PAIR * PAIRS as u64);
}

/// The memory plan of `lzma-recons` ([`crate::module::decode`]): of its initial memory
/// (`initial` bytes), parameters `params`, inputs of `inputs` bytes, an output of `len`, and the
/// inputs themselves.
pub fn decode(initial: u64, params: &[u8], inputs: &[u64], len: u64, files: [&[u8]; 3]) -> Option<u64> {
    let (mut m, streams) = start(initial, params, inputs, len, files)?;
    for (n, record) in streams {
        let h = recons::head_of(record, n)?;
        let top = m.top;
        // (`recons::join_onto`: the parts, the encoder's model, the match finder, the walk's)
        if h.parts > 1 {
            m.take(PART * h.parts as u64);
        }
        m.take(2 * lit_len(h.props) as u64);
        walk(&mut m, n, h.dict);
        m.top = top;
    }
    Some(m.plan())
}

/// The memory plan of `lzma2-recons` ([`crate::module::decode2`]), as [`decode`] for segments.
pub fn decode2(initial: u64, params: &[u8], inputs: &[u64], len: u64, files: [&[u8]; 3]) -> Option<u64> {
    let (mut m, streams) = start(initial, params, inputs, len, files)?;
    for (n, record) in streams {
        let h = lzma2::head_of(record, n)?;
        let top = m.top;
        let lzma = h.chunks.iter().any(|c| !c.stored());
        // (`lzma2::join_onto`: the chunks, the writer's model, the breaks, then the walk's, and an
        // LZMA chunk's coder in it)
        m.take(lzma2::wasm::CHUNK * h.chunks.len() as u64);
        if lzma {
            m.take(2 * LIT_MAX as u64);
        }
        m.take(lzma2::wasm::BREAK * h.chunks.len() as u64);
        walk(&mut m, n, h.dict);
        if lzma {
            m.take(CHUNK_ROOM as u64);
        }
        m.top = top;
    }
    Some(m.plan())
}
