//! aqr-lzx as a module of entry formulas (see aqr-formula): streams of LZX chunks rebuilt from
//! their data and records. `FORMAT.md` specifies it. The data is an input of its own, which the
//! formula may recompress in turn; a container (aqr-wim) gives the streams and places the
//! outputs.

use aqr_formula::{Apply, Formula, Kind, Node};

use crate::recons;

pub use crate::module::{decode, index};

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

/// The outputs, each a stream of chunks, of the application of [`MODULE`] to `streams` in `f`:
/// each its data (a node of its data's bytes), its record and its bytes. The records and the
/// index are streams added to `f`.
pub fn apply(f: &mut Formula, streams: Vec<(Node, Vec<u8>, u64)>) -> Vec<Node> {
    let lens: Vec<(usize, usize)> = streams.iter().map(|(d, r, _)| (d.len() as usize, r.len())).collect();
    let (mut data, mut records, mut sizes) = (Vec::new(), Vec::new(), Vec::new());
    for (d, r, n) in streams {
        data.push(d);
        records.extend_from_slice(&r);
        sizes.push(n);
    }
    let record = f.push_as("lzx/record", Kind::Compressed, records);
    let index = f.push("lzx/index", index(&lens));
    let a = Apply {
        module: MODULE,
        resources: vec![],
        params: vec![],
        inputs: vec![Node::concat(data), record, index],
        len: 0,
        memory_limit: aqr_formula::NO_LIMIT,
    };
    f.split(a, sizes)
}
