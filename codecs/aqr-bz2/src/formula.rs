//! aqr-bz2 as modules of entry formulas (see aqr-formula): bzip2 streams, one after another,
//! that libbzip2 compresses their data into ([`MODULE`]), and ALZip's streams of the same
//! compressor framed otherwise ([`MODULE_ALZ`], for aqr-alz). `FORMAT.md` specifies them. The
//! data is the input, which the formula may recompress in turn.

use aqr_common::pool;
use aqr_formula::{Apply, Formula, Module, Node};

pub use crate::Framing;
use crate::module::{StreamParams, get_framed, get_params, put_framed, put_params};
use crate::{compress, stream};

pub use crate::module::{decode, decode_alz};

pub const MODULE: Module = Module::new("bz2-recons", 1).with_plan(memory);

/// ALZip's bzip2 streams (`FORMAT.md`, §8).
pub const MODULE_ALZ: Module = Module::new("bz2-alz-recons", 1).with_plan(memory_alz);

/// Bytes of data taken, at most (a stream of zeros grows forty-thousandfold).
pub const MAX_DATA: usize = 1 << 30;

/// The memory of the module: its initial memory, and its plan ([`crate::module::plan`]), of
/// the data (whose blocks' symbols it reads).
fn memory(u: &aqr_formula::Usage<'_>) -> Option<u64> {
    memory_framed(u, Framing::Bzip2)
}

fn memory_alz(u: &aqr_formula::Usage<'_>) -> Option<u64> {
    memory_framed(u, Framing::Alz)
}

fn memory_framed(u: &aqr_formula::Usage<'_>, framing: Framing) -> Option<u64> {
    let (&[len], [data]) = (u.outputs, u.all_contents::<1>()?) else { return None };
    Some(u.initial() + crate::module::plan(framing, u.params, data, len)?)
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
    encode_if(input, threads, &mut |_, _, _| true)
}

/// Bytes of data decoded first, of a stream that has more: [`encode_if`] asks of them before
/// decoding the rest.
pub const PREFIX: usize = 4 << 20;

/// [`encode`], each stream taken only where `pays(coded, data, whole)` tells that it may pay,
/// given the data and the bytes of the stream it was coded in: first of the first [`PREFIX`]
/// bytes of data or so (`whole` false, not asked where that is all of it), before the rest is
/// decoded, then of all of it (`whole`), before it is compressed again to check it, which costs
/// most. The first stream not taken and the rest are sliced as they are.
pub fn encode_if(input: &[u8], threads: usize, pays: &mut dyn FnMut(usize, &[u8], bool) -> bool) -> Option<Formula> {
    let (mut at, mut streams, mut data) = (0, Vec::new(), Vec::new());
    while starts(&input[at..]) {
        let Some((sp, len)) = take(&input[at..], Framing::Bzip2, threads, MAX_DATA - data.len(), pays, &mut data)
        else {
            break;
        };
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

/// A stream of ALZip's bzip2 that is the whole of `d`: [`take_whole`] of [`Framing::Alz`].
pub fn take_alz(
    d: &[u8],
    threads: usize,
    max_data: usize,
    pays: &mut dyn FnMut(usize, &[u8], bool) -> bool,
) -> Option<(StreamParams, Vec<u8>)> {
    take_whole(d, Framing::Alz, threads, max_data, pays)
}

/// A stream of `framing` that is the whole of `d`, of at most `max_data` bytes of data, taken
/// only where `pays` tells (as [`encode_if`] asks): its parameters and its data, where
/// compressing that data gives `d` back.
pub fn take_whole(
    d: &[u8],
    framing: Framing,
    threads: usize,
    max_data: usize,
    pays: &mut dyn FnMut(usize, &[u8], bool) -> bool,
) -> Option<(StreamParams, Vec<u8>)> {
    let mut data = Vec::new();
    let (sp, len) = take(d, framing, threads, max_data.min(MAX_DATA), pays, &mut data)?;
    (len == d.len()).then_some((sp, data))
}

/// The application of [`MODULE_ALZ`] giving `streams` one after another (`len` bytes), of their
/// data `data` (one after another).
pub fn apply_alz(streams: &[StreamParams], data: Node, len: u64) -> Apply {
    apply_framed(Framing::Alz, streams, data, len)
}

/// The application of the module of `framing` ([`MODULE`] or [`MODULE_ALZ`]) giving `streams`
/// one after another (`len` bytes), of their data `data` (one after another).
pub fn apply_framed(framing: Framing, streams: &[StreamParams], data: Node, len: u64) -> Apply {
    let mut params = Vec::new();
    put_framed(&mut params, framing, streams);
    debug_assert_eq!(get_framed(&params, framing).as_deref(), Ok(streams));
    Apply {
        module: match framing {
            Framing::Bzip2 => MODULE,
            Framing::Alz => MODULE_ALZ,
        },
        resources: vec![],
        params,
        inputs: vec![data],
        len,
        memory_limit: aqr_formula::NO_LIMIT,
        task_limits: Vec::new(),
    }
}

/// The stream of `framing` at the start of `d`, its data appended to `data`, where compressing
/// that data gives it back; its parameters and bytes.
fn take(
    d: &[u8],
    framing: Framing,
    threads: usize,
    max_data: usize,
    pays: &mut dyn FnMut(usize, &[u8], bool) -> bool,
    data: &mut Vec<u8>,
) -> Option<(StreamParams, usize)> {
    let p = stream::Partial::start_framed(d, framing, PREFIX, max_data).ok()?;
    if p.ended.is_none() && !pays(p.coded(), &p.data, false) {
        return None;
    }
    let s = p.finish(threads, max_data).ok()?;
    let mut cuts = compress::boundaries(&s.data, s.level);
    cuts.push(s.data.len());
    let sizes: Vec<usize> = cuts.windows(2).map(|w| w[1] - w[0]).collect();
    if sizes != s.sizes || !pays(s.len, &s.data, true) {
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
        let mut b = compress::block_framed(at(0), framing);
        fix(0, &mut b);
        if !b.matches(d, if framing == Framing::Alz { 0 } else { 32 }) {
            return None;
        }
        blocks.push(b);
        let rest = pool::par_map(n - 1, threads, |k| compress::block_framed(at(k + 1), framing));
        for (k, mut b) in rest.into_iter().enumerate() {
            fix(k + 1, &mut b);
            blocks.push(b);
        }
    }
    if compress::stream_framed(framing, s.level, &blocks) != d[..s.len] {
        return None;
    }
    let len = s.data.len() as u64;
    data.extend_from_slice(&s.data);
    Some((StreamParams { level: s.level, len, orig_ptrs }, s.len))
}
