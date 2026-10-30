//! aqr-wasm as a module of entry formulas (see aqr-formula): the Wasm module rebuilt from its
//! streams ([`crate::wasm`]), for the archive to compress, the data segments' bytes and the
//! custom sections as content, to be recompressed in turn.

use aqr_formula::{Formula, Kind, Module, Node};

use crate::wasm::{self, CUSTOM, DATA, STREAM_NAMES, STREAMS};

pub use crate::module::decode;

pub const MODULE: Module = Module::new("wasm-recons", 1).with_memory(memory);

/// The memory of the module: the streams and the module (whose sections and functions are
/// made in place), and the list of locals of the function being made.
fn memory(u: &aqr_formula::Usage<'_>) -> Option<u64> {
    Some(u.whole(0, 0))
}

/// The classes of the streams, in order.
const CLASSES: [&str; STREAMS] = {
    let mut c = [""; STREAMS];
    let mut i = 0;
    while i < STREAMS {
        c[i] = match STREAM_NAMES[i].as_bytes() {
            b"skeleton" => "wasm/skeleton",
            b"sections" => "wasm/sections",
            b"custom" => "wasm/custom",
            b"locals" => "wasm/locals",
            b"op" => "wasm/op",
            b"local" => "wasm/local",
            b"label" => "wasm/label",
            b"brtable" => "wasm/brtable",
            b"blocktype" => "wasm/blocktype",
            b"offset" => "wasm/offset",
            b"i64" => "wasm/i64",
            b"bytes" => "wasm/bytes",
            b"misc" => "wasm/misc",
            b"data" => "wasm/data",
            _ => panic!("a stream without a class"),
        };
        i += 1;
    }
    c
};

/// Whether `d` starts like a Wasm module of the binary format's version 1.
pub fn starts(d: &[u8]) -> bool {
    d.starts_with(&wasm::HEADER)
}

/// The bytes of the Wasm module at the start of `d` (found in other data): its sections while
/// their ids are those of the binary format (to 13, the tag section) and they end within `d`.
pub fn extent(d: &[u8]) -> Option<usize> {
    if !starts(d) {
        return None;
    }
    let mut at = wasm::HEADER.len();
    while d.get(at).is_some_and(|&id| id <= 13) {
        let mut p = at + 1;
        let Ok(size) = aqr_common::varint::get(d, &mut p) else { break };
        match usize::try_from(size).ok().and_then(|n| p.checked_add(n)).filter(|&e| e <= d.len() && p - at <= 6) {
            Some(end) => at = end,
            None => break,
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
            if i == DATA || i == CUSTOM { f.push_content(CLASSES[i], Kind::Unknown, s) } else { f.push(CLASSES[i], s) }
        })
        .collect();
    f.root = Node::apply(MODULE, Vec::new(), inputs, input.len() as u64);
    Some(f)
}
