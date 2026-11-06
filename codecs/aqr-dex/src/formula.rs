//! aqr-dex as a module of entry formulas (see aqr-formula): the file rebuilt from its streams
//! (FORMAT.md), which the archive compresses.

use aqr_formula::{Formula, Kind, Module, Node};

use crate::dex::INPUT_NAMES;
use crate::encode;

pub use crate::module::decode;

pub const MODULE: Module = Module::new("dex-recons", 2).with_plan(memory);

/// The memory of the module: its initial memory, and its plan ([`crate::module::plan`]).
fn memory(u: &aqr_formula::Usage<'_>) -> Option<u64> {
    Some(u.initial() + crate::module::plan(u.params.len() as u64, u.inputs, u.content(0)?, u.output())?)
}

/// Whether `d` starts like a DEX file.
pub fn starts(d: &[u8]) -> bool {
    encode::starts(d)
}

/// The length of the DEX file at the start of `d`, by its header.
pub fn extent(d: &[u8]) -> Option<usize> {
    starts(d).then(|| u32::from_le_bytes(d[0x20..0x24].try_into().unwrap()) as usize)
}

/// The formula of DEX file `input`, or `None` where it is not one this crate takes. It is the
/// same whatever `threads` is (one is used).
pub fn encode(input: &[u8], _threads: usize) -> Option<Formula> {
    let st = encode::split(input)?;
    let mut f = Formula::default();
    let inputs = st
        .into_iter()
        .enumerate()
        .map(|(i, data)| {
            // (the first three hold sections as they are)
            let kind = if i < 3 { Kind::Unknown } else { Kind::Table };
            f.push_as(format!("dex/{}", INPUT_NAMES[i]), kind, data)
        })
        .collect();
    f.root = Node::apply(MODULE, Vec::new(), inputs, input.len() as u64);
    Some(f)
}
