//! aqr-snappy as a module of entry formulas (see aqr-formula): a Snappy stream compressed from
//! its data, or a LevelDB table rebuilt from the contents of its blocks. `FORMAT.md` specifies
//! it. The data, and the contents of the blocks, may be recompressed in turn.

use aqr_common::varint;
use aqr_formula::{Formula, Module, Node};

use crate::leveldb::{self, CODE_KEPT, CODE_RAW, CODE_SNAPPY, RAW, SNAPPY};
use crate::snappy::{self, Hash};
use crate::{Error, Result, corrupt};

pub const MODULE: Module = Module { name: "snappy-recons", version: 1 };

const STREAM: u8 = 0;
const TABLE: u8 = 1;

/// The application of the module making the Snappy stream of `len` bytes that `data` compresses
/// into by `hash`.
pub fn stream(hash: Hash, data: Node, len: u64) -> Node {
    Node::apply(MODULE, vec![STREAM, hash as u8], vec![data], len)
}

/// The hash that compresses `data` into `stream`, if any does.
pub fn find_hash(data: &[u8], stream: &[u8]) -> Option<Hash> {
    Hash::ALL.into_iter().find(|&h| snappy::compress(data, h).is_ok_and(|s| s == stream))
}

/// The formula rebuilding the LevelDB table `input`, or None where it is not one, or none of its
/// blocks is a Snappy stream the compressor gives. Blocks not rebuilt are kept as they are.
pub fn encode(input: &[u8], _threads: usize) -> Option<Formula> {
    let blocks = leveldb::blocks(input).ok()?;
    let stored = |b: &leveldb::Block| &input[b.offset..b.offset + b.size];
    // One compressor writes a table: the first block that tells the hashes apart decides.
    let mut hash = None;
    let mut decoded = Vec::with_capacity(blocks.len());
    for b in &blocks {
        let d = (b.kind == SNAPPY).then(|| snappy::decode(stored(b), 1 << 30).ok()).flatten();
        if hash.is_none()
            && let Some(d) = &d
        {
            let hits: Vec<Hash> =
                Hash::ALL.into_iter().filter(|&h| snappy::compress(d, h).is_ok_and(|s| s == stored(b))).collect();
            if hits.len() == 1 {
                hash = Some(hits[0]);
            }
        }
        decoded.push(d);
    }
    let hash = hash.unwrap_or(Hash::Mul);
    let (mut data, mut layout, mut kept) = (Vec::new(), Vec::new(), Vec::new());
    let mut rebuilt = false;
    for (b, d) in blocks.iter().zip(decoded) {
        let s = stored(b);
        match d {
            Some(d) if snappy::compress(&d, hash).is_ok_and(|c| c == s) => {
                varint::put(&mut layout, d.len() as u64);
                layout.push(CODE_SNAPPY);
                data.extend_from_slice(&d);
                rebuilt = true;
            }
            _ if b.kind == RAW => {
                varint::put(&mut layout, s.len() as u64);
                layout.push(CODE_RAW);
                data.extend_from_slice(s);
            }
            _ => {
                varint::put(&mut layout, s.len() as u64);
                layout.extend_from_slice(&[CODE_KEPT, b.kind]);
                kept.extend_from_slice(s);
            }
        }
    }
    if !rebuilt || leveldb::build(&data, &layout, &kept, hash).ok()? != input {
        return None;
    }
    let mut f = Formula::default();
    let inputs = vec![f.push("ldb/blocks", data), f.push("ldb/layout", layout), f.push("ldb/kept", kept)];
    f.root = Node::apply(MODULE, vec![TABLE, hash as u8], inputs, input.len() as u64);
    Some(f)
}

/// The module: a Snappy stream or a LevelDB table of `len` bytes (see `FORMAT.md`).
pub fn decode(params: &[u8], inputs: &[&[u8]], len: u64, _threads: usize) -> Result<Vec<u8>> {
    let &[kind, h] = params else { return Err(corrupt()) };
    let hash = Hash::from_u8(h).ok_or_else(corrupt)?;
    let out = match (kind, inputs) {
        (STREAM, &[data]) => snappy::compress(data, hash)?,
        (TABLE, &[data, layout, kept]) => leveldb::build(data, layout, kept, hash)?,
        _ => return Err(corrupt()),
    };
    if out.len() as u64 != len {
        return Err(Error("output of another length"));
    }
    Ok(out)
}
