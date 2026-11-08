//! The memory plans of the modules `lzma-recons` and `lzma2-recons` on WebAssembly
//! ([`aqr_api::stack`]), as [`crate::module::decode_part`] lays them out: the instance's memory
//! taken block by block in the order the modules take it ([`Sim`]). The modules are part
//! modules, spawn no tasks and hold nothing of the lengths of their inputs and output but what a
//! stream's dictionary asks for: the window of its data (the dictionary and some slack, or the
//! stream where it is shorter), the match finder's rings and tables, and the walk's contexts.
//! What the plan reads of the inputs is each stream's length and its record's start (the
//! dictionary size, the match finder's reach and, for LZMA, the properties).
//!
//! The encoder chooses the finder's reach ([`finder_for`]) as the largest that the application's
//! memory bound allows: the rings follow it, not the dictionary.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

use aqr_api::stack::Sim;

use crate::lzma::{Props, lit_len};
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
/// lay it out: the window of its data (the dictionary's reach), the match finder (of the reach
/// `finder`), (the encoder's) and then the walk's contexts and pairs.
fn stream(m: &mut Sim, n: usize, dict: u32, finder: usize, own: impl FnOnce(&mut Sim)) {
    m.take(window::room(n, recons::reach(dict)) as u64);
    for b in mf::blocks(n, finder) {
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
        stream(&mut m, n, h.dict, recons::finder_of(h.dict, h.fin)?, |m| own_lzma(m, h.props));
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
        stream(&mut m, n, h.dict, recons::finder_of(h.dict, h.fin)?, own_lzma2);
        if n > 0 {
            m.peak(CHUNK_ROOM as u64);
        }
        m.top = top;
    }
    Some(m.plan())
}

/// `StreamEncoder::to`'s: the coder's piece, then the model's literal probabilities.
fn own_lzma(m: &mut Sim, props: Props) {
    m.take(PIECE as u64);
    m.take(2 * lit_len(props) as u64);
}

/// `Writer::to`'s: the model's literal probabilities, for the largest properties.
fn own_lzma2(m: &mut Sim) {
    m.take(2 * LIT_MAX as u64);
}

/// What an application of a module takes besides its streams': the initial memory of the module
/// (17 pages, the largest of the two) and the windows of its data, records and output (16 pages
/// each at most; the index's is a page or none), as `memory::limit` of aqr-compress adds them to
/// the plan. `lzma_finder_fits_the_bound` of aqr-decompress's tests checks it against them.
pub const AROUND: u64 = (17 + 3 * 16) * 65536;

/// The memory bound of the application that [`finder_for`] keeps to, by default none.
static BOUND: AtomicU64 = AtomicU64::new(u64::MAX);

/// Sets the memory bound that the encoders keep the plans of their applications to, by choosing
/// the match finders' reach ([`finder_for`]); the compressor's application bound.
pub fn set_bound(bound: u64) {
    BOUND.store(bound, Relaxed);
}

/// What a stream's coder takes, for [`finder_for`].
#[derive(Clone, Copy)]
pub enum Own {
    /// An LZMA stream of these properties.
    Lzma(Props),
    /// A segment of an LZMA2 stream.
    Lzma2,
}

/// The memory limit of an application of a module over one stream of `n` bytes of data of a
/// dictionary of `dict` bytes, its match finder of the reach `finder` ([`recons::finder_of`]):
/// the plan with what is around ([`AROUND`]).
pub fn application(n: usize, dict: u32, finder: usize, own: Own) -> u64 {
    let mut m = Sim::new(AROUND);
    m.take(INDEX_PIECE as u64);
    m.take(PIECE as u64);
    match own {
        Own::Lzma(props) => stream(&mut m, n, dict, finder, |m| own_lzma(m, props)),
        Own::Lzma2 => {
            stream(&mut m, n, dict, finder, own_lzma2);
            m.peak(CHUNK_ROOM as u64);
        }
    }
    m.plan()
}

/// The reach of the match finder to record for a stream of `n` bytes of data of a dictionary of
/// `dict` bytes ([`recons::finder_of`]'s byte): 0, the dictionary's, where the application with
/// it fits the bound ([`set_bound`]) or no reach would, else the largest power of two below it
/// that does fit. A smaller reach finds fewer far matches, so the record costs more (a match
/// beyond it is coded as a far distance), but the rings of 12 bytes for each position are
/// smaller.
pub fn finder_for(n: usize, dict: u32, own: Own) -> u8 {
    let bound = BOUND.load(Relaxed);
    let fits = |finder: usize| application(n, dict, finder, own) <= bound;
    let reach = recons::reach(dict);
    if bound == u64::MAX || fits(reach) {
        return 0;
    }
    // (the powers of two below the reach)
    let mut k = usize::BITS - 1 - (reach - 1).leading_zeros();
    while k >= 12 {
        if fits(1 << k) {
            return k as u8;
        }
        k -= 1;
    }
    0
}
