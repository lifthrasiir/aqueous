//! The formula of a `.clip` file: the module `clip-recons` (see FORMAT.md) applied to the
//! skeleton, the index, the tiles' streams and the database (aqr-sqlite's formula embedded where
//! it takes it, else the database as content of that format).

use crate::dynamic::{self, Escape, Model};
use crate::module::{self, MAX_BATCH, Piece, batches, layouts, pieces, sources};
use crate::tile::{self, AREA, Layout};
use aqr_common::arith::ArithEncoder;
use aqr_common::{pool, varint};
use aqr_formula::{Apply, Formula, Kind, Module, Node, PAGE};
use std::collections::HashMap;

pub use crate::module::decode;

pub const MODULE: Module = Module::new("clip-recons", 1).with_memory(memory);

/// Tiles a batch holds: a stream each, decoded on a thread of its own.
pub const BATCH: usize = 64;

/// Task slots of the module (its bin's `whole!(slots 8, ..)`): batches running at once.
const SLOTS: usize = 8;

/// Bytes of a [`Model`] on 32 bits: 4-byte bits, 4 x 128 of copies, 10 x 256 of samples, 329
/// Elias-gamma codes of 156 and the list of 257 pixels.
const MODEL: u64 = 4 * (4 * 128 + 5 + 10 * 256 + 329 * 156) + 8 * (dynamic::LIST as u64 + 1);

/// The window, heads and links of zlib's emulator ([`crate::zlib::PARAMS`]: windowBits 15 and
/// memLevel 9) and its other state.
const EMULATOR: u64 = 2 * 32768 + 280 + 2 * 65536 + 2 * 32768 + 1024;

/// Tokens of a block (6 bytes each).
const TOKENS: u64 = 32767;

/// The memory of the module, on 32 bits, from what its index and skeleton say (the walk of
/// [`pieces`] is made again). The batches decode as tasks, each on a copy of the spawner's memory
/// as it is at the spawn (`W`: the inputs and what the index makes, the vectors of the pieces
/// and batches counted at their growth), so at most [`SLOTS`] tasks, the largest ones, take `W`
/// and their own at once, while the spawner holds all the results taken (the tile streams, and 40
/// bytes a tile for the vectors holding them) and the bytes they came in (freed, but not taken
/// to be reused). A task holds, deciding a tile, the larger of:
/// - the pixels (8 bytes each), the tile (its layout's bytes), the model, the emulator, the
///   block's tokens, the writer's output (which grows by 6 bytes a token on top of the stream
///   so far, to twice the need at most, the old held while the new is made: three times) and the
///   streams so far (all of the batch's), 16 KiB for the rest;
/// - the streams and the bytes they are put in (their sizes and 8 bytes a tile; pushed to
///   from empty, the old capacity and the new held at the last growth), to give them back.
///
/// With one batch there are no tasks (the spawner decodes it) nor bytes. The output is made at
/// its size after them, with the references to the distinct tiles (8 bytes each).
fn memory(u: &aqr_formula::Usage<'_>) -> Option<u64> {
    let &[skeleton, index, _, _] = u.contents else { return None };
    let mut at = 0;
    let per =
        varint::get(u.params, &mut at).ok().filter(|&p| at == u.params.len() && (1..=MAX_BATCH as u64).contains(&p))?;
    let mut at = 0;
    let num = |at: &mut usize| varint::get(index, at).ok().and_then(|v| usize::try_from(v).ok());
    let nruns = num(&mut at).filter(|&n| n <= index.len())?;
    let runs: Vec<usize> = (0..nruns).map(|_| num(&mut at)).collect::<Option<_>>()?;
    let (mut run, mut left, mut on) = (0, runs.first().copied().unwrap_or(0), true);
    let mut flags = |_: Layout, _: &[u8]| -> bool {
        while left == 0 && run + 1 < runs.len() {
            run += 1;
            left = runs[run];
            on = !on;
        }
        if left == 0 {
            return false;
        }
        left -= 1;
        on
    };
    let ps = pieces(skeleton, false, &mut flags).ok()?;
    let all = layouts(&ps);
    let nrefs = num(&mut at).filter(|&n| n <= index.len())?;
    let (mut refs, mut next) = (Vec::with_capacity(nrefs), 0usize);
    for _ in 0..nrefs {
        let p = next.checked_add(num(&mut at)?).filter(|&p| p < all.len())?;
        refs.push((p, num(&mut at)?));
        next = p + 1;
    }
    let (distinct, srcs) = sources(&all, &refs).ok()?;
    let bs = batches(&distinct, per as usize);
    // the length of each distinct tile's stream
    let mut zlen = vec![0u64; distinct.len()];
    let mut tiles = srcs.iter();
    for p in &ps {
        if let Piece::Tile(_, n) = *p {
            zlen[*tiles.next()?] = n as u64;
        }
    }
    let (d, nb) = (distinct.len() as u64, bs.len() as u64);
    let w = u.base()
        + 32 * ps.len() as u64
        + 8 * all.len() as u64
        + 16 * nruns as u64
        + 8 * nrefs as u64
        + 24 * d
        + 100 * nb
        + 256;
    let (mut results, mut stale, mut tasks) = (0u64, 0u64, Vec::new());
    for (l, places) in &bs {
        let n = places.len() as u64;
        let sum: u64 = places.iter().map(|&i| zlen[i]).sum();
        let raw = places.iter().map(|&i| zlen[i].saturating_sub(6)).max().unwrap_or(0);
        let held = sum + 40 * n + 28;
        let wire = 9 + 8 * n + sum;
        let out = if wire <= 8 { 16 } else { 3 * (wire.next_power_of_two() / 2) };
        let deciding = 8 * AREA as u64 + l.bytes() as u64 + MODEL + EMULATOR + 6 * TOKENS + 3 * (raw + 6 * TOKENS + 16);
        let big = |x: u64| x.min(PAGE);
        let pads = big(8 * AREA as u64) + big(l.bytes() as u64) + 3 * PAGE + big(sum) + big(raw);
        let task = (deciding + held).max(held + out) + (16 << 10) + pads;
        results += held;
        stale += wire + 16;
        tasks.push(task);
    }
    tasks.sort_unstable_by(|a, b| b.cmp(a));
    let slack = (2 * nb + 4) * PAGE;
    let (spawn, stale) = match tasks.len() {
        0 => (0, 0),
        1 => (tasks[0], 0),
        _ => (tasks.iter().take(SLOTS).map(|t| w + t).sum(), stale),
    };
    let at_spawn = w + results + stale + spawn + slack;
    let at_end = w + results + stale + 8 * d + u.output() + slack;
    Some(at_spawn.max(at_end))
}

fn inflate(z: &[u8], len: usize) -> Option<Vec<u8>> {
    use flate2::{Decompress, FlushDecompress, Status};
    let mut out = Vec::with_capacity(len);
    let st = Decompress::new(true).decompress_vec(z, &mut out, FlushDecompress::Finish).ok()?;
    (st == Status::StreamEnd && out.len() == len).then_some(out)
}

/// The formula of `input`, or `None` where it is not a `.clip` file whose structure holds (or
/// a panic is caught). A tile is modelled where its zlib stream is what [`crate::zlib`] makes
/// of it.
pub fn encode(input: &[u8], threads: usize) -> Option<Formula> {
    std::panic::catch_unwind(|| encode_at(input, threads)).ok().flatten()
}

fn encode_at(input: &[u8], threads: usize) -> Option<Formula> {
    // the blocks that may be modelled, then which are
    let mut found: Vec<(Layout, &[u8])> = Vec::new();
    pieces(input, true, &mut |l, z| {
        found.push((l, z));
        false
    })
    .ok()?;
    // (the tiles are inflated again when coded, rather than held: they are 40 times the file)
    let ok: Vec<bool> = pool::par_map(found.len(), threads, |i| {
        let (l, z) = found[i];
        inflate(z, l.bytes()).and_then(|t| crate::zlib::deflate(&t)).is_some_and(|again| again == z)
    });
    let mut next = ok.iter().copied();
    let ps = pieces(input, true, &mut |_, _| next.next().unwrap_or(false)).ok()?;

    // the skeleton and the database
    let (mut skeleton, mut database, mut at) = (Vec::new(), &input[..0], 0);
    for p in &ps {
        match *p {
            Piece::Keep(n) => {
                skeleton.extend_from_slice(&input[at..at + n]);
                at += n;
            }
            Piece::Tile(_, n) => at += n,
            Piece::Database(n) => {
                database = &input[at..at + n];
                at += n;
            }
        }
    }
    if at != input.len() {
        return None;
    }

    // the runs of modelled and kept blocks, from modelled
    let mut runs = vec![0usize];
    for &m in &ok {
        if m != (runs.len() % 2 == 1) {
            runs.push(0);
        }
        *runs.last_mut().unwrap() += 1;
    }
    // a tile equal to an earlier one (the same stream) is a reference to it
    let modelled: Vec<&[u8]> = found.iter().zip(&ok).filter(|(_, ok)| **ok).map(|(f, _)| f.1).collect();
    let mut first: HashMap<&[u8], usize> = HashMap::new();
    let (mut refs, mut distinct) = (Vec::new(), Vec::new());
    for (i, &z) in modelled.iter().enumerate() {
        match first.get(z) {
            Some(&d) => refs.push((i, distinct.len() - d)),
            None => {
                first.insert(z, distinct.len());
                distinct.push(z);
            }
        }
    }
    let (layouts_of, _) = sources(&layouts(&ps), &refs).ok()?;
    let modelled = distinct;
    let bs = batches(&layouts_of, BATCH);
    let streams = pool::par_map(bs.len(), threads, |b| {
        let (l, places) = &bs[b];
        let mut m = Model::new(Escape::Med, true);
        let mut e = dynamic::Enc(ArithEncoder::new(Vec::new()));
        let mut px = Vec::with_capacity(AREA);
        for &i in places {
            tile::pixels(*l, &inflate(modelled[i], l.bytes()).expect("inflated before"), &mut px);
            m.tile(&mut e, *l, &mut px);
        }
        e.0.finish()
    });
    let mut index = Vec::new();
    varint::put(&mut index, runs.len() as u64);
    for &r in &runs {
        varint::put(&mut index, r as u64);
    }
    varint::put(&mut index, refs.len() as u64);
    let mut next = 0;
    for &(i, back) in &refs {
        varint::put(&mut index, (i - next) as u64);
        varint::put(&mut index, back as u64);
        next = i + 1;
    }
    for s in &streams {
        varint::put(&mut index, s.len() as u64);
    }
    let mut params = Vec::new();
    varint::put(&mut params, BATCH as u64);

    let mut f = Formula::default();
    let skeleton = f.push_content("clip/skeleton", Kind::Unknown, skeleton);
    let index = f.push("clip/index", index);
    let tiles = f.push_as("clip/tiles", Kind::Compressed, streams.concat());
    let database = if database.is_empty() {
        f.push("clip/database", Vec::new())
    } else {
        match aqr_sqlite::formula::encode(database, threads) {
            Some(child) => f.embed(child),
            None => f.push_content("clip/database", Kind::Format("sqlite"), database.to_vec()),
        }
    };
    f.root = Node::apply(MODULE, params, vec![skeleton, index, tiles, database], input.len() as u64);
    Some(f)
}

/// Evaluates an application of this module or of aqr-sqlite's (for tests and evaluators).
pub fn apply(a: &Apply, inputs: &[&[u8]], threads: usize) -> Result<Vec<u8>, String> {
    if a.module == MODULE {
        module::decode(&a.params, inputs, a.len, threads).map_err(|e| e.0)
    } else if a.module == aqr_sqlite::formula::MODULE {
        aqr_sqlite::formula::decode(&a.params, inputs, a.len, threads).map_err(|e| format!("{e:?}"))
    } else {
        Err(format!("unknown module {}", a.module))
    }
}
