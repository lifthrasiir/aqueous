//! aqr-arsc as a module of entry formulas (see aqr-formula): the file rebuilt from its inputs
//! (FORMAT.md), which the archive compresses.

use aqr_formula::{Formula, Kind, Module, Node};

use crate::arsc::INPUT_NAMES;
use crate::encode;

pub use crate::module::decode;

pub const MODULE: Module = Module::new("arsc-recons", 1).with_memory(memory);

/// The memory of the module: the file, and what earlier entries predict (at most the file).
fn memory(u: &aqr_formula::Usage<'_>) -> Option<u64> {
    let len = u.outputs.iter().sum::<u64>();
    Some(u.whole(len, 2))
}

/// Whether `d` starts like a resource table.
pub fn starts(d: &[u8]) -> bool {
    encode::starts(d)
}

/// The length of the resource table at the start of `d`, by its header.
pub fn extent(d: &[u8]) -> Option<usize> {
    (d.len() >= 12 && d[..4] == [2, 0, 12, 0]).then(|| u32::from_le_bytes(d[4..8].try_into().unwrap()) as usize)
}

/// The formula of resource table `input`, or `None` where it is not one this crate takes. It
/// is the same whatever `threads` is (one is used).
pub fn encode(input: &[u8], _threads: usize) -> Option<Formula> {
    let st = encode::split(input)?;
    let mut f = Formula::default();
    let inputs = st
        .into_iter()
        .enumerate()
        .map(|(i, data)| {
            let kind = match INPUT_NAMES[i] {
                "text" => Kind::Text,
                "head" => Kind::Unknown,
                _ => Kind::Table,
            };
            f.push_as(format!("arsc/{}", INPUT_NAMES[i]), kind, data)
        })
        .collect();
    f.root = Node::apply(MODULE, Vec::new(), inputs, input.len() as u64);
    Some(f)
}
