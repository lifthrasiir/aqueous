//! git pack recompression: a pack is rebuilt bit for bit from the data of its objects, the zlib
//! streams by aqr-flate, so that the data is stored instead. `formula` is the entry point (see
//! aqr-formula); `FORMAT.md` specifies the module.
//!
//! What the archive does for any file covers the rest of a git checkout: loose objects are
//! zlib streams of their own, and a blob's data is the file checked out, which the archive
//! finds again once the pack's data is laid bare. Only the entries' headers that follow from
//! their data, the zlib headers and checksums and the pack's trailer are derived (`layout`);
//! `pack` reads a pack into its entries.

// The code of the module alone uses only part of the rest.
#![cfg_attr(not(feature = "encode"), allow(unused))]

#[cfg(feature = "encode")]
pub mod formula;
pub mod layout;
pub mod module;
pub mod pack;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error(pub &'static str);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for Error {}

impl From<aqr_common::Corrupt> for Error {
    fn from(_: aqr_common::Corrupt) -> Self {
        corrupt()
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[cold]
pub fn corrupt() -> Error {
    Error("corrupt input")
}
