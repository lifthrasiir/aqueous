//! aqr-bz2 as a module of entry formulas (see aqr-formula): bzip2 streams, one after another,
//! that libbzip2 compresses their data into. `FORMAT.md` specifies it. The data is the input,
//! which the formula may recompress in turn.

use aqr_common::pool;
use aqr_formula::{Formula, Module, Node};

use crate::module::{StreamParams, get_params, put_params};
use crate::{compress, stream};

pub use crate::module::decode;

pub const MODULE: Module = Module::new("bz2-recons", 1).with_memory(memory);

/// Bytes of data taken, at most (a stream of zeros grows forty-thousandfold).
pub const MAX_DATA: usize = 1 << 30;

/// Bytes of a block's work for each of its symbols: the block written twice and its suffix
/// array, the types and the reduced text of SA-IS, then RLE1, the BWT and the MTF symbols.
const WORK_PER_SYMBOL: u64 = 2 + 4 * 2 + 2 + 4 * 2 + 2 + 1 + 2;

/// The memory of the module: the input and the output, the blocks compressed before they are
/// joined (as large as the output), and a block's work.
fn memory(u: &aqr_formula::Usage<'_>) -> Option<u64> {
    let &[data] = u.inputs else { return None };
    let streams = get_params(u.params).ok()?;
    let level = streams.iter().map(|s| s.level).max()? as u64;
    let syms = (100_000 * level).min(data + data / 4 + 5);
    let out = u.outputs.iter().sum::<u64>();
    Some(u.whole(WORK_PER_SYMBOL * syms + out, 12))
}

/// Whether `d` starts with a bzip2 stream, as far as its header and first magic tell.
pub fn starts(d: &[u8]) -> bool {
    stream::level(d).is_some()
        && d.get(4..10).is_some_and(|m| {
            let m = m.iter().fold(0u64, |a, &b| a << 8 | b as u64);
            m == crate::BLOCK_MAGIC || m == crate::END_MAGIC
        })
}

/// Bytes of the bzip2 stream at the start of `d`, its symbols decoded but not its data; None
/// where it does not decode.
pub fn extent(d: &[u8]) -> Option<usize> {
    if !starts(d) {
        return None;
    }
    stream::decode(d, false, usize::MAX).ok().map(|s| s.len)
}

/// The formula rebuilding `input`: the bzip2 streams at its start, one after another, that
/// libbzip2 gives for their data, and what follows them sliced as it is. None where the first
/// is not one.
pub fn encode(input: &[u8], threads: usize) -> Option<Formula> {
    encode_if(input, threads, &mut |_, _| true)
}

/// [`encode`], each stream taken only where `pays` tells, given the bytes of the stream and its
/// data, that it may pay: before its data is compressed again to check it, which costs most.
/// The first stream not taken and the rest are sliced as they are.
pub fn encode_if(input: &[u8], threads: usize, pays: &mut dyn FnMut(usize, &[u8]) -> bool) -> Option<Formula> {
    let (mut at, mut streams, mut data) = (0, Vec::new(), Vec::new());
    while starts(&input[at..]) {
        let Some((sp, len)) = take(&input[at..], threads, MAX_DATA - data.len(), pays, &mut data) else { break };
        streams.push(sp);
        at += len;
    }
    if streams.is_empty() {
        return None;
    }
    let mut params = Vec::new();
    put_params(&mut params, &streams);
    debug_assert_eq!(get_params(&params).as_deref(), Ok(&streams[..]));
    let mut f = Formula::default();
    f.push("bz2", input[at..].to_vec());
    let data = f.push("raw", data);
    f.root = Node::concat([
        Node::apply(MODULE, params, vec![data], at as u64),
        Node::slice(0, 0, (input.len() - at) as u64),
    ]);
    Some(f)
}

/// The stream at the start of `d`, its data appended to `data`, where compressing that data
/// gives it back; its parameters and bytes.
fn take(
    d: &[u8],
    threads: usize,
    max_data: usize,
    pays: &mut dyn FnMut(usize, &[u8]) -> bool,
    data: &mut Vec<u8>,
) -> Option<(StreamParams, usize)> {
    let s = stream::decode(d, true, max_data).ok()?;
    let mut cuts = compress::boundaries(&s.data, s.level);
    cuts.push(s.data.len());
    let sizes: Vec<usize> = cuts.windows(2).map(|w| w[1] - w[0]).collect();
    if sizes != s.sizes || !pays(s.len, &s.data) {
        return None;
    }
    let n = sizes.len();
    let at = |k: usize| &s.data[cuts[k]..cuts[k + 1]];
    let mut orig_ptrs = Vec::new();
    let mut fix = |k: usize, b: &mut compress::Block| {
        if b.orig_ptr != s.orig_ptrs[k] {
            b.set_orig_ptr(s.orig_ptrs[k]);
            orig_ptrs.push((k, s.orig_ptrs[k]));
        }
    };
    // the first block first, so that another compressor's stream costs only that
    let mut blocks = Vec::with_capacity(n);
    if n > 0 {
        let mut b = compress::block(at(0));
        fix(0, &mut b);
        if !b.matches(d, 32) {
            return None;
        }
        blocks.push(b);
        for (k, mut b) in pool::par_map(n - 1, threads, |k| compress::block(at(k + 1))).into_iter().enumerate() {
            fix(k + 1, &mut b);
            blocks.push(b);
        }
    }
    if compress::stream(s.level, &blocks) != d[..s.len] {
        return None;
    }
    let len = s.data.len() as u64;
    data.extend_from_slice(&s.data);
    Some((StreamParams { level: s.level, len, orig_ptrs }, s.len))
}
