//! aqr-bvx as a module of entry formulas (see aqr-formula): a stream of bvx blocks rebuilt
//! from its data and record. `FORMAT.md` specifies it. The data is an input of its own, which
//! the formula may recompress in turn.

use aqr_formula::{Formula, Kind, Module, Node};

use crate::{bvx, recons};

pub use crate::module::decode;

pub const MODULE: Module = Module::new("bvx-recons", 1).with_memory(memory);

/// Bytes of data of a stream taken, at most (a stream of zeros grows a thousandfold).
pub const MAX_DATA: u64 = 1 << 30;

/// The memory of the module: the inputs and the output, the match finder's and the table's
/// arrays, the parse (a match of 12 bytes for 4 bytes of data or so) and a block being written
/// (its literals, at most 315 for each of its triples, and its triples).
fn memory(u: &aqr_formula::Usage<'_>) -> Option<u64> {
    let &[data, _] = u.inputs else { return None };
    let block = (bvx::L_MAX as u64 + 12) * recons::BLOCK_LMD as u64;
    Some(u.whole(recons::MF_BYTES + recons::TABLE_BYTES + 3 * data + block.min(2 * data + (1 << 16)), 2))
}

/// Whether `d` starts with a stream of bvx blocks, as far as their headers tell.
pub fn starts(d: &[u8]) -> bool {
    d.starts_with(b"bvx") && bvx::extent(d).is_some()
}

/// Bytes of the stream at the start of `d`, up to and with its end block; None where its
/// blocks' headers do not hold.
pub fn extent(d: &[u8]) -> Option<usize> {
    bvx::extent(d).map(|e| e.0)
}

/// The formula rebuilding `input`, a stream of bvx blocks and what follows it, or None where
/// it is not one, holds no data or does not split. What follows the stream is sliced as it is.
pub fn encode(input: &[u8], _threads: usize) -> Option<Formula> {
    let (_, n) = bvx::extent(input).filter(|e| (1..=MAX_DATA).contains(&e.1))?;
    let sp = recons::split(input, n as usize).ok()?;
    let mut f = Formula::default();
    f.push("bvx", input[sp.used..].to_vec());
    let data = f.push("raw", sp.data);
    let record = f.push_as("bvx/record", Kind::Compressed, sp.record);
    f.root = Node::concat([
        Node::apply(MODULE, vec![], vec![data, record], sp.used as u64),
        Node::slice(0, 0, (input.len() - sp.used) as u64),
    ]);
    Some(f)
}
