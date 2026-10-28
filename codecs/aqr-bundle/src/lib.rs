//! Archives that only bundle files, without compressing them: ar (System V and GNU, BSD,
//! Windows libraries), tar (v7, ustar, GNU, pax) and cpio (newc, crc, odc, binary). An archive
//! is rebuilt bit for bit from the contents of its members, so that each content is a stream of
//! its own to recompress, deduplicate or compress with its like. `formula` is the entry point
//! (see aqr-formula); `FORMAT.md` specifies the module.
//!
//! The headers are kept as they are, but the fields that follow from the contents (sizes, name
//! lengths, checksums, padding) are stored as their difference to that prediction (`layout`),
//! so whatever the writer did is kept exactly. `ar`, `tar` and `cpio` read the archives into
//! members, with whatever lies between or after them kept as gaps.

// The code of the modules alone uses only part of the rest.
#![cfg_attr(not(feature = "encode"), allow(unused))]
pub mod ar;
pub mod cpio;
#[cfg(feature = "encode")]
pub mod formula;
pub mod layout;
pub mod module;
pub mod tar;

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
