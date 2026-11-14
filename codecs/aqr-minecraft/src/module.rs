//! The code of the module `mca-recons` (see `formula`, and FORMAT.md), which needs no
//! aqr-formula, so that aqr-modules builds it into its Wasm module. It is a part module: the
//! inputs taken in order in pieces and the file put in order, a chunk at a time; natively it is
//! applied whole, the chunks' data made again in order and their zlib streams on threads.

use std::collections::VecDeque;

use aqr_api::stack::{Sim, block};
use aqr_api::stream::{In, Put, Reader, Slice, Written};
use aqr_flate::plan::wasm::{JOB, LANE, TOKEN, WINDOW_SLACK};
use aqr_flate::zlib::Params;

use crate::layout::{self, Chunks, Holes, Made, PIECE, SMALL_PIECE, Source, Streamed};
use crate::region::{CHUNKS, HEADER};
use crate::{Result, corrupt, model, nbt};

/// Pages of the windows of the module as a part module ([`decode_part`]): of the data, the
/// bytes kept, the holes coded and the output; and of the header and the layout.
pub const WINDOW: u32 = 16;
pub const SMALL_WINDOW: u32 = 1;

/// Bytes of a [`crate::region::Chunk`] on WebAssembly.
const CHUNK: u64 = 8;

/// The memory plan of `mca-recons` as a part module ([`aqr_api::stack`]): what [`decode_part`]
/// takes beyond its initial memory (`initial` bytes), of the header `header`, the layout `layout`
/// and an output of `size` bytes: the header, the pieces of the readers, the model and its
/// buffers ([`Holes`]), the chunks, then for the largest chunk its sectors (rebuilt), its data,
/// and its holes coded or zlib's emulator. Of the largest chunk, not of the file.
pub fn plan(initial: u64, params: &[u8], header: &[u8], layout: &[u8], size: u64) -> Option<u64> {
    let p = layout::params(params).ok()?;
    let made = layout::made(header.get(..HEADER)?, layout, size).ok()?;
    let mut m = Sim::new(initial);
    m.take(HEADER as u64);
    m.take(SMALL_PIECE as u64);
    m.take(PIECE as u64);
    m.take(PIECE as u64);
    m.take(PIECE as u64);
    for b in model::memory().into_iter().chain(model::work_memory()) {
        m.take(b);
    }
    m.take(core::mem::size_of::<nbt::Section>() as u64 * nbt::SECTIONS as u64);
    m.take(core::mem::size_of::<nbt::Frame>() as u64 * nbt::DEPTH as u64);
    m.take(CHUNK * CHUNKS as u64);
    let mut most = 0;
    for c in &made {
        let coded = c.coded.map_or(0, block);
        let held = match c.rebuilt {
            true => block(c.sectors + aqr_flate::write::TOKEN_SLACK as u64) + block(c.n) + coded.max(emulator(p, c.n)),
            false if c.coded.is_some() => block(c.n) + coded,
            false => 0,
        };
        most = most.max(held);
    }
    m.peak(most);
    Some(m.plan())
}

/// The blocks that zlib's emulator takes over `n` bytes of data: its window, its heads and links,
/// a lane and a job, and the tokens of a block.
fn emulator(p: Params, n: u64) -> u64 {
    let w = 1u64 << p.wbits.clamp(9, 15);
    let bits = p.mem.clamp(1, 9) as u32 + 7;
    let tokens = block(TOKEN * (p.block_tokens() as u64).min(n + 1));
    block(2 * w + WINDOW_SLACK) + block(2 << bits) + block(2 * w) + block(LANE) + block(JOB) + tokens
}

/// Bytes of chunks' data made again before their zlib streams are made at once.
const BATCH: u64 = 32 << 20;

/// The module: the region file of `len` bytes made of `inputs`: the header, the layout, the
/// chunks' data, the bytes kept and the holes coded; the chunks' zlib streams made on up to
/// `threads` threads.
pub fn decode(params: &[u8], inputs: &[&[u8]], len: u64, threads: usize) -> Result<Vec<u8>> {
    let &[header, layout, data, kept, coded] = inputs else { return Err(corrupt()) };
    let p = layout::params(params)?;
    if header.len() != HEADER {
        return Err(corrupt());
    }
    let made = layout::made(header, layout, len)?;
    let mut out = Vec::with_capacity(usize::try_from(len).map_err(|_| corrupt())?);
    let (mut d, mut c) = (Slice(data), Slice(coded));
    let src = Source {
        data: Reader::new(&mut d, PIECE),
        coded: Reader::new(&mut c, PIECE),
        left: data.len() as u64,
        coded_left: coded.len() as u64,
        holes: Holes::new(),
    };
    let mut chunks = Batched { src, params: p, threads, made: made.into_iter(), ready: VecDeque::new() };
    let (mut layout, mut kept) = (Slice(layout), Slice(kept));
    let (mut layout, mut kept) = (Reader::new(&mut layout, SMALL_PIECE), Reader::new(&mut kept, PIECE));
    layout::build_stream(header, &mut layout, &mut chunks, &mut kept, len, &mut out)?;
    Ok(out)
}

/// The chunks' data of a [`Source`], made again a batch at a time in order, then their zlib
/// streams (of those rebuilt) on threads.
struct Batched<'a> {
    src: Source<'a>,
    params: Params,
    threads: usize,
    made: std::vec::IntoIter<Made>,
    ready: VecDeque<Result<Vec<u8>>>,
}

impl Batched<'_> {
    /// The next chunk's stream (rebuilt) or data (stored).
    fn next(&mut self) -> Result<Vec<u8>> {
        if self.ready.is_empty() {
            let mut batch = Vec::new();
            let mut bytes = 0;
            while bytes < BATCH
                && let Some(c) = self.made.next()
            {
                bytes += c.n;
                batch.push((c.rebuilt, self.src.next(c.n, c.coded)?));
            }
            let p = self.params;
            let made = aqr_common::pool::par_map(batch.len(), self.threads, |i| match &batch[i] {
                (true, d) => layout::zlib_onto(d, p, Vec::new()),
                (false, _) => Ok(Vec::new()),
            });
            for ((rebuilt, d), z) in batch.into_iter().zip(made) {
                self.ready.push_back(if rebuilt { z } else { Ok(d) });
            }
        }
        self.ready.pop_front().ok_or_else(corrupt)?
    }
}

impl Chunks for Batched<'_> {
    fn rebuilt(&mut self, _: u64, _: Option<u64>, mut out: Vec<u8>) -> Result<Vec<u8>> {
        out.extend_from_slice(&self.next()?);
        Ok(out)
    }

    fn stored(&mut self, _: u64, _: Option<u64>, out: &mut dyn Put) -> Result<()> {
        out.put(&self.next()?).ok_or_else(corrupt)
    }

    fn done(&mut self) -> bool {
        self.ready.is_empty() && self.made.len() == 0 && self.src.done()
    }
}

/// The module as a part module (its entry point, of `aqr_api::entry!`): what [`decode`] gives,
/// its inputs taken and its output put in order through windows ([`WINDOW`]), a chunk at a time.
pub fn decode_part(
    params_len: u64,
    header_len: u64,
    layout_len: u64,
    data_len: u64,
    kept_len: u64,
    coded_len: u64,
    size: u64,
) {
    aqr_api::finish(part(params_len, [header_len, layout_len, data_len, kept_len, coded_len], size));
}

fn part(params_len: u64, lens: [u64; 5], size: u64) -> Result<()> {
    use aqr_api::Memory;
    let mut p = [0; 4];
    if params_len != 4 || lens[0] != HEADER as u64 {
        return Err(corrupt());
    }
    aqr_api::Params::read(0, &mut p);
    let params = layout::params(&p)?;
    let header = layout::read_header(&mut In::<0>::new(lens[0]))?;
    let (mut l, mut d, mut k, mut c) =
        (In::<1>::new(lens[1]), In::<2>::new(lens[2]), In::<3>::new(lens[3]), In::<4>::new(lens[4]));
    let mut layout = Reader::new(&mut l, SMALL_PIECE);
    let data = Reader::new(&mut d, PIECE);
    let mut kept = Reader::new(&mut k, PIECE);
    let coded = Reader::new(&mut c, PIECE);
    let mut out = Written::new(size);
    let src = Source { data, coded, left: lens[2], coded_left: lens[4], holes: Holes::new() };
    let mut chunks = Streamed { src, params };
    layout::build_stream(&header, &mut layout, &mut chunks, &mut kept, size, &mut out)?;
    match out.total() == size {
        true => Ok(()),
        false => Err(corrupt()),
    }
}
