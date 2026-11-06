//! aqr-ima as a module of entry formulas (see aqr-formula): IMA ADPCM blocks rebuilt from the
//! record of their headers and nibbles (FORMAT.md).

use aqr_formula::{Formula, Module, Node};

use crate::codec::{self, Layout};
use crate::module;

pub use crate::module::decode;

pub const MODULE: Module = Module::new("ima-recons", 2).with_plan(memory);

/// The memory of the module: its initial memory, and its plan ([`crate::module::plan`]).
fn memory(u: &aqr_formula::Usage<'_>) -> Option<u64> {
    let (&[record], &[len]) = (u.inputs, u.outputs) else { return None };
    Some(u.initial() + crate::module::plan(u.params, record, len)?)
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
