//! The formula of a `.clip` file: the module `clip-recons` (see FORMAT.md) applied to the
//! skeleton, the index, the tiles' streams and the database (aqr-sqlite's formula embedded where
//! it takes it, else the database as content of that format).

use crate::dynamic::{self, Escape, Model};
use crate::module::{self, Piece, batches, layouts, pieces};
use crate::tile::{self, AREA, Layout};
use aqr_common::arith::ArithEncoder;
use aqr_common::{pool, varint};
use aqr_formula::{Apply, Formula, Kind, Module, Node};

pub use crate::module::decode;

pub const MODULE: Module = Module { name: "clip-recons", version: 1 };

/// Tiles a batch holds: a stream each, decoded on a thread of its own.
pub const BATCH: usize = 64;

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
    let modelled: Vec<&[u8]> = found.iter().zip(&ok).filter(|(_, ok)| **ok).map(|(f, _)| f.1).collect();
    let bs = batches(&layouts(&ps), BATCH);
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
