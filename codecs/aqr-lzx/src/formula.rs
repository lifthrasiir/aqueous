//! aqr-lzx as a module of entry formulas (see aqr-formula): streams of LZX chunks rebuilt from
//! their data and records, with bytes kept between them. `FORMAT.md` specifies it. The data is
//! an input of its own, which the formula may recompress in turn; a container (aqr-wim) gives
//! the streams and the bytes between them, so that an application's output is a part of the
//! container as it is.

use aqr_formula::{Apply, Formula, Kind, Node};

use crate::recons;

pub use crate::module::{Part, decode, index};

pub const MODULE: aqr_formula::Module = aqr_formula::Module::new("lzx-recons", 1).with_memory(memory);

/// Bytes the module works in besides its inputs and output: a chunk's match finder (chains of
/// 2 and 3 bytes), its data and its parse (a token at most a byte, of 6 bytes), and 64 KiB for
/// the rest (contexts, trees).
pub const WORK: u64 = (2 * 65536 + 2 * recons::CHUNK) as u64 * 4 + 7 * recons::CHUNK as u64 + (64 << 10);

fn memory(u: &aqr_formula::Usage<'_>) -> Option<u64> {
    Some(u.whole(WORK, 1))
}

/// The streams split on up to `threads` threads, each a stream of chunks (their bytes and
/// their data's lengths, see [`recons::split`]): None for those that do not split.
pub fn split(streams: &[Vec<(&[u8], usize)>], threads: usize) -> Vec<Option<recons::Split>> {
    aqr_common::pool::par_map(streams.len(), threads, |i| recons::split(&streams[i]).ok())
}

/// A stream of an application: the bytes kept before it (a node), its data (a node of its
/// data's bytes) and its record.
pub struct Stream {
    pub kept: Node,
    pub data: Node,
    pub record: Vec<u8>,
}

/// The application of [`MODULE`] in `f` whose output is each of `streams` after the bytes
/// kept before it, then `tail`: `len` bytes in all. Its records and index are streams added
/// to `f`.
pub fn apply(f: &mut Formula, streams: Vec<Stream>, tail: Node, len: u64) -> Node {
    let parts: Vec<crate::module::Part> = streams
        .iter()
        .map(|s| crate::module::Part {
            kept: s.kept.len() as usize,
            data: s.data.len() as usize,
            record: s.record.len(),
        })
        .collect();
    let (mut data, mut kept, mut records) = (Vec::new(), Vec::new(), Vec::new());
    for s in streams {
        kept.push(s.kept);
        data.push(s.data);
        records.extend_from_slice(&s.record);
    }
    kept.push(tail);
    let record = f.push_as("lzx/record", Kind::Compressed, records);
    let index = f.push("lzx/index", index(&parts));
    Node::Apply(Box::new(Apply {
        module: MODULE,
        resources: vec![],
        params: vec![],
        inputs: vec![Node::concat(data), record, Node::concat(kept), index],
        len,
        memory_limit: aqr_formula::NO_LIMIT,
    }))
}
