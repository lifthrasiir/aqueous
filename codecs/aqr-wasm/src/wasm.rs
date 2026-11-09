//! A WebAssembly module split into streams and joined back (FORMAT.md): the code is
//! `aqr_common::wasm`, shared with aqr-formula, whose module blocks hold the same split form.

use crate::{Result, corrupt};
pub use aqr_common::wasm::{CUSTOM, DATA, HEADER, STREAM_NAMES, STREAMS, split};
use aqr_common::wasm::{L32, Mtf};

/// What [`join`] takes of an [`aqr_api::stack`] at most, of a module of `len` bytes: the module
/// (with the room of two sizes), and on top of it a function's or a segment's list of local
/// indices ([`Mtf`]), or the size of what was made.
pub fn join_plan(len: u64) -> u64 {
    use aqr_api::stack::block;
    block(len.min(1 << 30) + 2 * L32 as u64) + block(4 * (Mtf::LEN as u64 + 1)).max(block(L32 as u64))
}

/// The module of streams `ins` ([`split`]), of `len` bytes.
pub fn join(ins: [&[u8]; STREAMS], len: u64) -> Result<Vec<u8>> {
    aqr_common::wasm::join(ins, len).ok_or_else(corrupt)
}
