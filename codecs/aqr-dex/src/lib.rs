//! Android DEX files rebuilt bit for bit (`FORMAT.md`): the strings front-coded, the tables of
//! ids and class definitions by column, the classes' data with the offsets of their code as
//! ranks, and the other sections each in a stream of its own, for the archive to compress.
//! `formula` is the entry point (see aqr-formula).

// The code of the module alone uses only part of the rest.
#![cfg_attr(not(feature = "encode"), allow(unused))]
pub mod code;
pub mod dex;
#[cfg(feature = "encode")]
pub mod encode;
#[cfg(feature = "encode")]
pub mod formula;
pub mod module;

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
