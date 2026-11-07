//! The memory plan of the module `ogg-recons` on WebAssembly ([`aqr_api::stack`]), as
//! [`crate::module::decode`] lays it out: the instance's memory taken block by block in the order
//! the module takes it ([`Sim`]): the copies of the inputs, the CRC's tables, the layout's lists,
//! the streams' packets (the count of what they hold, from the inputs), the walk's list and the
//! output.

use aqr_api::stack::Sim;

use crate::layout::{Layout, Stream};
use crate::module::{self, wasm};

/// Bytes of the CRC's tables (`crc::tables`).
const CRC_TABLES: u64 = 8 * 256 * 4;

/// The memory plan of `ogg-recons`: of its initial memory (`initial` bytes), inputs of these
/// lengths (the layout record `layout` and the packets `packets`), and an output of `len`.
pub fn decode(initial: u64, inputs: &[u64], len: u64, layout: &[u8], packets: &[u8]) -> Option<u64> {
    if inputs.len() != 2 {
        return None;
    }
    let mut m = Sim::new(initial);
    m.take(aqr_api::stack::whole(0, &[], inputs));
    m.take(len);
    m.take(CRC_TABLES);
    let l = Layout::read(layout, len).ok()?;
    let streams = l.streams.len() as u64;
    m.take(size_of::<Stream>() as u64 * streams);
    m.take(wasm::PAGE * l.pages.len() as u64);
    // `Streams::read`: the bytes, the ends, the lacing values, the durations, the starts
    let t = module::totals(&l, packets).ok()?;
    m.take(t.bytes as u64);
    m.take(4 * t.packets as u64);
    m.take(t.lacing as u64);
    m.take(8 * t.packets as u64);
    m.take(12 * (streams + 1));
    m.take(wasm::WALKED * streams);
    Some(m.plan(1))
}
