//! aqr-wasm as a module of entry formulas (see aqr-formula): the Wasm module rebuilt from its
//! streams ([`crate::wasm`]), for the archive to compress, the data segments' bytes and the
//! custom sections as content, to be recompressed in turn.

use aqr_formula::{Formula, Kind, Module, Node};

use crate::wasm::{self, CUSTOM, DATA, STREAM_NAMES};

pub use crate::module::decode;

pub const MODULE: Module = Module::new("wasm-recons", 1).with_plan(memory);

/// The memory of the module: its initial memory, and its plan ([`crate::module::plan`]).
fn memory(u: &aqr_formula::Usage<'_>) -> Option<u64> {
    Some(u.initial() + crate::module::plan(u.inputs, u.output()))
}

/// Whether `d` starts like a Wasm module of the binary format's version 1.
pub fn starts(d: &[u8]) -> bool {
    d.starts_with(&wasm::HEADER)
}

/// The bytes of the Wasm module at the start of `d` (found in other data): its sections while
/// their ids are those of the binary format (to 13, the tag section), in the order the format
/// gives (custom sections anywhere) and each once, and they end within `d`.
pub fn extent(d: &[u8]) -> Option<usize> {
    if !starts(d) {
        return None;
    }
    // (the place of each id in the order: the tag section (13) comes after memory, the data
    // count (12) before code)
    const ORDER: [u8; 14] = [0, 1, 2, 3, 4, 5, 7, 8, 9, 10, 12, 13, 11, 6];
    let mut at = wasm::HEADER.len();
    let mut last = 0;
    while let Some(&id) = d.get(at).filter(|&&id| id <= 13) {
        let rank = ORDER[id as usize];
        if id != 0 && rank <= last {
            break;
        }
        let mut p = at + 1;
        let Ok(size) = aqr_common::varint::get(d, &mut p) else { break };
        match usize::try_from(size).ok().and_then(|n| p.checked_add(n)).filter(|&e| e <= d.len() && p - at <= 6) {
            Some(end) => at = end,
            None => break,
        }
        if id != 0 {
            last = rank;
        }
    }
    Some(at)
}

/// The formula of Wasm module `input`, or `None` where it is not one this crate takes
/// ([`wasm::split`]). The same whatever `threads` is.
pub fn encode(input: &[u8], _threads: usize) -> Option<Formula> {
    let streams = wasm::split(input)?;
    let mut f = Formula::default();
    let inputs = streams
        .into_iter()
        .enumerate()
        .map(|(i, s)| {
            let class = format!("wasm/{}", STREAM_NAMES[i]);
            if i == DATA || i == CUSTOM { f.push_content(class, Kind::Unknown, s) } else { f.push(class, s) }
        })
        .collect();
    f.root = Node::apply(MODULE, Vec::new(), inputs, input.len() as u64);
    Some(f)
}
