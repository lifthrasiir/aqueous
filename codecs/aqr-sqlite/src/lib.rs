//! Lossless SQLite 3 database recompression: a database file is split into streams, and
//! rebuilt from them bit for bit. `formula` is the entry point (a module of aqr-formula).

#![allow(clippy::needless_range_loop)]
// The code of the modules alone uses only part of the rest.
#![cfg_attr(not(feature = "encode"), allow(unused))]

#[cfg(feature = "encode")]
pub mod formula;
pub mod index;
pub mod module;
pub mod page;
pub mod par;
pub mod record;
pub mod transform;
pub mod varint;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error(pub &'static str);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for Error {}

/// As the error of a task (decoding on WebAssembly): the message is not kept.
impl aqr_common::wire::Wire for Error {
    fn put(&self, _: &mut Vec<u8>) {}

    fn get(_: &mut &[u8]) -> Option<Self> {
        Some(Error("(in a task)"))
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[cold]
pub fn corrupt() -> Error {
    Error("corrupt input")
}

pub use aqr_common::mem;
