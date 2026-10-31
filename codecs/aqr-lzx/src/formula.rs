//! aqr-lzx as a module of entry formulas (see aqr-formula): streams of LZX chunks rebuilt from
//! their data and records, with bytes kept between them. `FORMAT.md` specifies it. The data is
//! an input of its own, which the formula may recompress in turn; a container (aqr-wim) gives
//! the streams and the bytes between them, so that an application's output is a part of the
//! container as it is.

use aqr_formula::{Apply, Formula, Kind, Node};

use crate::{lzx, recons};

pub use crate::module::{Part, decode, index};

pub const MODULE: aqr_formula::Module = aqr_formula::Module::new("lzx-recons", 1).with_memory(memory);

/// Bytes the module works in besides its inputs and output for a stream of `n` bytes of data
/// with a window of `window` bytes: the match finder (chains of 2 and 3 bytes), its data and
/// its parse (a token at most a byte), and 64 KiB for the rest (contexts, trees).
pub const fn work(window: usize, n: usize) -> u64 {
    (2 * 65536 + 2 * window) as u64 * 4 + (n * (1 + std::mem::size_of::<lzx::Tok>())) as u64 + (64 << 10)
}

/// [`work`] of WIM's chunks.
pub const WORK: u64 = work(recons::CHUNK, recons::CHUNK);

fn memory(u: &aqr_formula::Usage<'_>) -> Option<u64> {
    let v = crate::module::variant(u.params).ok()?;
    if !v.cab {
        return Some(u.whole(WORK, 1));
    }
    // (CAB's streams one after another: the largest's, and its output once more as it is written)
    let parts = crate::module::parts(u.content(3)?).ok()?;
    let n = parts.iter().map(|p| p.data).max().unwrap_or(0);
    Some(u.whole(work(v.window(), n) + n as u64, 1))
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
    }
}
