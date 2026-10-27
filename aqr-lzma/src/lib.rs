//! LZMA streams split into their data and a record of their parse, and rebuilt bit for bit
//! (`recons`), so that the data is stored instead: once, where other files hold it too.
//! `formula` is the entry point (see aqr-formula), with the feature `encode`; `module` is the
//! code of the module, which `FORMAT.md` specifies.
//!
//! The container taken is the `.lzma` file (LZMA-alone, `alone`).

// The code of the modules alone uses only part of the rest.
#![cfg_attr(not(feature = "encode"), allow(unused))]
pub mod alone;
pub mod arith;
#[cfg(feature = "encode")]
pub mod formula;
pub mod lzma;
pub mod mf;
pub mod module;
pub mod rc;
pub mod recons;

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
