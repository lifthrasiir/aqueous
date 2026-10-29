//! aqr-ima as a module of entry formulas (see aqr-formula): IMA ADPCM blocks rebuilt from the
//! record of their headers and nibbles (FORMAT.md).

use aqr_formula::{Formula, Module, Node};

use crate::codec::{self, Layout};
use crate::module;

pub use crate::module::decode;

pub const MODULE: Module = Module::new("ima-recons", 2).with_memory(memory);

/// The memory of the module: the record and the blocks; the model (its nibbles' contexts, 16 x
/// 17 x 17 x 16 bits of 4 bytes, and a few per channel); a block's nibbles (two a byte of each
/// channel's) and the block.
fn memory(u: &aqr_formula::Usage<'_>) -> Option<u64> {
    let mut at = 0;
    let channels = aqr_common::varint::get(u.params, &mut at).ok()?;
    let per_channel = aqr_common::varint::get(u.params, &mut at).ok()?;
    let block = Layout::new(channels as usize, per_channel as usize)?.block() as u64;
    Some(u.whole(4 * 16 * 17 * 17 * 16 + 64 * channels + 3 * block, 2))
}

/// The node rebuilding the whole blocks of `l` at the start of `data`, and the bytes it takes;
/// None where there is no whole block.
pub fn blocks(f: &mut Formula, l: Layout, data: &[u8]) -> Option<(Node, usize)> {
    let (used, record) = codec::encode(l, data);
    if used == 0 {
        return None;
    }
    let inputs = vec![f.push("ima/record", record)];
    Some((Node::apply(MODULE, module::params(l), inputs, used as u64), used))
}
