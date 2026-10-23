//! ZIP recompression: an archive is rebuilt bit for bit from the contents of its entries, the
//! DEFLATE streams by aqr-flate, so that the contents are stored instead. `formula` is the entry
//! point (see aqr-formula); `FORMAT.md` specifies the module.
//!
//! The headers are kept as they are, but every field that follows from the contents or from
//! another header (CRCs, sizes, offsets, what the central directory repeats of the local
//! headers) is stored as its difference to that prediction (`layout`), so whatever the writer
//! did is kept exactly. `zip` reads the archive into records, with whatever lies between them
//! kept as gaps.

pub mod formula;
pub mod layout;
pub mod zip;

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
