//! aqr-lz4 as a module of entry formulas (see aqr-formula): the LZ4 block or frame that
//! liblz4 compresses its input into, with the compressor's parameters. `FORMAT.md` specifies
//! it. The input is the data itself, which the formula may recompress in turn.

use aqr_common::varint;
use aqr_formula::{Formula, Module, Node};

use crate::block::{self, Compressor, MAX_ACCELERATION};
use crate::frame;
use crate::module::{BLOCK, FRAME};

pub use crate::module::decode;

pub const MODULE: Module = Module::new("lz4-recons", 1).with_memory(memory);

/// The memory of the module: the data and the stream, the compressor's table, and for a frame
/// a block compressed apart (of at most 4 MiB of data) before it is appended.
fn memory(u: &aqr_formula::Usage<'_>) -> Option<u64> {
    let &[data] = u.inputs else { return None };
    let block = match u.params.first() {
        Some(&FRAME) => block::bound(data.min(4 << 20) as usize) as u64,
        _ => 0,
    };
    Some(u.whole(block::TABLE_BYTES + block, 2))
}

/// Firefox's container: this, the data's length (u32, little endian), then a block.
pub const MOZLZ4: [u8; 8] = *b"mozLz40\0";

/// Accelerations tried, in order: `LZ4_compress_default` (and the default level of frames)
/// first, then those of `lz4 --fast` and other common choices.
pub const ACCELERATIONS: [u32; 15] = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 12, 16, 17, 32, MAX_ACCELERATION];

/// Whether `d` starts like a container this module rebuilds the stream of.
pub fn starts(d: &[u8]) -> bool {
    d.starts_with(&MOZLZ4) || d.starts_with(&frame::MAGIC)
}

/// Bytes of the container at the start of `d`: a `mozlz4` file up to the end of its block, or
/// a frame; None where its structure does not hold.
pub fn extent(d: &[u8]) -> Option<usize> {
    if d.starts_with(&MOZLZ4) {
        let n = u32::from_le_bytes(*d.get(8..)?.first_chunk()?) as usize;
        Some(12 + block::end(&d[12..], n)?)
    } else {
        frame::end(d)
    }
}

/// The formula rebuilding `input`, or None where it is not a container this module takes or
/// its stream is not one the compressor gives. What follows the container is sliced as it is.
pub fn encode(input: &[u8], _threads: usize) -> Option<Formula> {
    if input.starts_with(&MOZLZ4) {
        mozlz4(input)
    } else if input.starts_with(&frame::MAGIC) {
        lz4_frame(input)
    } else {
        None
    }
}

fn mozlz4(d: &[u8]) -> Option<Formula> {
    let n = u32::from_le_bytes(*d.get(8..)?.first_chunk()?) as usize;
    let end = 12 + block::end(&d[12..], n)?;
    let blk = &d[12..end];
    let mut data = Vec::with_capacity(n);
    block::decode(blk, &mut data, 0, n).ok()?;
    let a = *ACCELERATIONS.iter().find(|&&a| Compressor::reproduces_fast(&data, a, blk))?;
    let mut params = vec![BLOCK];
    varint::put(&mut params, a as u64);
    let mut f = Formula::default();
    f.push("lz4", [&d[..12], &d[end..]].concat());
    let data = f.push("raw", data);
    f.root = Node::concat([
        Node::slice(0, 0, 12),
        Node::apply(MODULE, params, vec![data], blk.len() as u64),
        Node::slice(0, 12, (d.len() - end) as u64),
    ]);
    Some(f)
}

fn lz4_frame(d: &[u8]) -> Option<Formula> {
    let fr = frame::decode(d).ok()?;
    if !frame::encodable(fr.flg, fr.bd) {
        return None;
    }
    let orig = &d[..fr.len];
    let a = *ACCELERATIONS
        .iter()
        .find(|&&a| matches!(frame::encode(&fr.data, fr.flg, fr.bd, a, Some(orig)), Ok(Some(_))))?;
    let mut params = vec![FRAME];
    varint::put(&mut params, a as u64);
    params.extend_from_slice(&[fr.flg, fr.bd]);
    let mut f = Formula::default();
    f.push("lz4", d[fr.len..].to_vec());
    let data = f.push("raw", fr.data);
    f.root = Node::concat([
        Node::apply(MODULE, params, vec![data], fr.len as u64),
        Node::slice(0, 0, (d.len() - fr.len) as u64),
    ]);
    Some(f)
}
