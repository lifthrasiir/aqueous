//! The memory plan of the module `clip-recons` on WebAssembly ([`aqr_api::stack`]), as
//! [`crate::module::decode`] lays it out: the instance's memory taken block by block in the order
//! the module takes it ([`Sim`]), and the tasks of the batches, each a copy of the instance's
//! memory as it spawns them and what it takes over it. What the module takes follows from its
//! index and skeleton (read here as the module reads them, [`parse`]), and the lengths of its
//! inputs and output.

use aqr_api::stack::{Sim, block};
use aqr_flate::plan::wasm::{JOB, LANE, TOKEN, WINDOW_SLACK};

use crate::dynamic::MODEL_BLOCKS;
use crate::module::{Batch, Parsed, parse, per_batch, wasm};
use crate::tile::AREA;
use crate::zlib::{PARAMS, ROOM};

/// Threads the module is given (its task slots).
pub const THREADS: usize = 8;

/// The blocks a batch of a task holds at the most, as `module::decode_batch` takes them: the
/// model, the pixels, a tile, the batch's streams (with room for the writer's), then for
/// deflating a tile zlib's emulator (the window, its heads and links, a lane and a job), and
/// the tokens of its first block.
fn task(p: &Parsed, b: &Batch) -> u64 {
    let model: u64 = MODEL_BLOCKS.iter().map(|&n| block(n as u64)).sum();
    let sum: u64 = p.order[b.from..b.from + b.n].iter().map(|&i| p.places[i].1 as u64).sum();
    let w = 1u64 << PARAMS.wbits.clamp(9, 15);
    let bits = PARAMS.mem.clamp(1, 9) as u32 + 7;
    let emulator = block(2 * w + WINDOW_SLACK) + block(2 << bits) + block(2 * w) + block(LANE) + block(JOB);
    let tokens = block(TOKEN * PARAMS.block_tokens().min(b.layout.bytes() + 1) as u64);
    model + block(8 * AREA as u64) + block(b.layout.bytes() as u64) + block(sum + ROOM as u64) + emulator + tokens
}

/// The memory plan of `clip-recons` ([`crate::module::decode`]): of its initial memory (`initial`
/// bytes), parameters `params`, inputs of `inputs` bytes, an output of `len`, the skeleton and the
/// index.
pub fn decode(initial: u64, params: &[u8], inputs: &[u64], len: u64, files: (&[u8], &[u8])) -> Option<(u64, u64)> {
    let (instance, tasks) = sim(initial, params, inputs, len, files)?.plans(1);
    Some((instance, tasks[0]))
}

/// [`decode`]'s [`Sim`], before its plan is taken.
pub fn sim(initial: u64, params: &[u8], inputs: &[u64], len: u64, (skeleton, index): (&[u8], &[u8])) -> Option<Sim> {
    let &[_, _, tiles, database] = inputs else { return None };
    let p =
        parse(per_batch(params).ok()?, skeleton, index, usize::try_from(database).ok()?, tiles as usize, len).ok()?;
    let mut m = Sim::new(initial);
    m.take(aqr_api::stack::whole(params.len() as u64, &[], inputs));
    // what parse reserves
    m.take(wasm::WORD * p.runs.len() as u64);
    m.take(wasm::PIECE * p.pieces.len() as u64);
    m.take(p.all.len() as u64);
    m.take(8 * p.refs.len() as u64);
    m.take(p.distinct.len() as u64);
    m.take(wasm::WORD * p.srcs.len() as u64);
    m.take(wasm::BATCH * p.batches.len() as u64);
    m.take(wasm::WORD * p.order.len() as u64);
    m.take(wasm::WORD * p.lens.len() as u64);
    m.take(wasm::WORD * p.starts.len() as u64);
    m.take(wasm::PLACE * p.places.len() as u64);
    let nb = p.batches.len() as u64;
    let sums = |b: &Batch| p.order[b.from..b.from + b.n].iter().map(|&i| p.places[i].1 as u64).sum::<u64>();
    if p.batches.len() >= 2 {
        // (the tasks copy the memory, then their handles are taken, the file, and each result
        // as it is joined)
        let tasks = p.batches.iter().map(|b| m.task(task(&p, b))).collect();
        m.take(nb);
        m.take(len);
        m.peak(p.batches.iter().map(|b| block(sums(b))).max().unwrap_or(0));
        m.spawned(0, tasks);
    } else {
        m.take(len);
        if let Some(b) = p.batches.first() {
            m.peak(task(&p, b));
        }
    }
    Some(m)
}
