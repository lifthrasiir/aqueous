//! Lossless SQLite 3 database recompression: a database file is split into streams, and
//! rebuilt from them bit for bit. `formula` is the entry point (a module of aqr-formula).

#![allow(clippy::needless_range_loop)]

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

pub type Result<T> = std::result::Result<T, Error>;

#[cold]
pub fn corrupt() -> Error {
    Error("corrupt input")
}

pub use aqr_common::mem;
