//! aqr-ac3 as a module of entry formulas (see aqr-formula): an AC-3 byte stream from its coded form. `FORMAT.md`
//! specifies it.
use std::vec::Vec;

use aqr_formula::Module;
use aqr_walk::rc::Encoder;

pub use crate::native::decode;
use crate::stream;

/// The class of the coded stream.
pub const CODED: &str = "ac3/frames";

/// The module `ac3-recons`: no params; the input the coded stream.
pub const MODULE: Module = Module::new("ac3-recons", 1).with_plan(memory);

fn memory(u: &aqr_formula::Usage<'_>) -> Option<u64> {
    let &[_] = u.inputs else { return None };
    Some(crate::module::plan(u.initial()))
}

/// The coded form of the byte stream `data` (chunks coded at once on `threads`), if it comes to less than 97% of it.
pub fn encode_with(data: &[u8], threads: usize) -> Option<Vec<u8>> {
    let out = encode_always(data, threads);
    (out.len() as f64 <= 0.97 * data.len() as f64).then_some(out)
}

/// The coded form of the byte stream `data` whatever its size (for tests).
pub fn encode_always(data: &[u8], threads: usize) -> Vec<u8> {
    let recs = stream::records(data);
    let chunks = stream::chunks(&recs);
    let parts = aqr_common::pool::par_map(chunks.len(), threads, |i| {
        let rs = &recs[chunks[i].clone()];
        let bytes = rs.last().unwrap().end - rs[0].start;
        let mut enc = Encoder::new();
        stream::encode_chunk(&mut enc, data, rs);
        (bytes, enc.finish())
    });
    let mut out = Vec::new();
    aqr_walk::chunks::put(&mut out, parts.iter().map(|(bytes, coded)| (*bytes as u64, coded.len() as u64)));
    for (_, coded) in &parts {
        out.extend_from_slice(coded);
    }
    out
}

/// [`encode_with`] on the threads of the machine.
pub fn encode(data: &[u8]) -> Option<Vec<u8>> {
    encode_with(data, std::thread::available_parallelism().map_or(1, |n| n.get()))
}
