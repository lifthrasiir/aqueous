//! aqr-lzx as a module of entry formulas (see aqr-formula): streams of LZX chunks rebuilt from
//! their data and records, with bytes kept between them. `FORMAT.md` specifies it. The data is
//! an input of its own, which the formula may recompress in turn; a container (aqr-wim) gives
//! the streams and the bytes between them, so that an application's output is a part of the
//! container as it is.

use aqr_formula::{Apply, Formula, Kind, Node};

use crate::{lzx, recons};

pub use crate::module::{Part, decode, index};

pub const MODULE: aqr_formula::Module = aqr_formula::Module::new("lzx-recons", 3).with_plan(plan).with_contents(&[0]);

/// The memory of the module: its plan ([`crate::plan::decode`]).
fn plan(u: &aqr_formula::Usage<'_>) -> Option<u64> {
    // (the records and the index: the data is not asked for, as it may be sunk)
    let files = [u.content(1)?, u.content(3)?];
    crate::plan::decode(u.initial(), u.params, u.inputs, files)
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
    Node::Apply(Box::new(apply_as(f, lzx::WIM, streams, tail, len)))
}

/// [`apply`] of streams of `v` (CAB's: their data e8 undone, as [`recons::split_stream`] gives
/// it), as an application.
pub fn apply_as(f: &mut Formula, v: lzx::Variant, streams: Vec<Stream>, tail: Node, len: u64) -> Apply {
    let lens: Vec<usize> = streams.iter().map(|s| s.data.len() as usize).collect();
    let (data, rest): (Vec<Node>, Vec<(Node, Vec<u8>)>) =
        streams.into_iter().map(|s| (s.data, (s.kept, s.record))).unzip();
    let parts = rest.into_iter().zip(lens).map(|((kept, record), n)| (kept, n, record)).collect();
    apply_joined(f, v, Node::concat(data), parts, tail, len)
}

/// [`apply_as`] of the streams' data one after another in `data`: each stream the bytes kept
/// before it, its bytes of data and its record.
pub fn apply_joined(
    f: &mut Formula,
    v: lzx::Variant,
    data: Node,
    streams: Vec<(Node, usize, Vec<u8>)>,
    tail: Node,
    len: u64,
) -> Apply {
    let parts: Vec<crate::module::Part> = streams
        .iter()
        .map(|s| crate::module::Part { kept: s.0.len() as usize, data: s.1, record: s.2.len() })
        .collect();
    let (mut kept, mut records) = (Vec::new(), Vec::new());
    for s in streams {
        kept.push(s.0);
        records.extend_from_slice(&s.2);
    }
    kept.push(tail);
    let record = f.push_as("lzx/record", Kind::Compressed, records);
    let index = f.push("lzx/index", index(&parts));
    Apply {
        module: MODULE,
        resources: vec![],
        params: if v.cab { vec![v.window_bits] } else { vec![] },
        inputs: vec![data, record, Node::concat(kept), index],
        len,
        memory_limit: aqr_formula::NO_LIMIT,
        task_limits: Vec::new(),
    }
}
