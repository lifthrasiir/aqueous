//! Snappy recompression: a Snappy stream is rebuilt bit for bit from its data, by Snappy's
//! compressor reimplemented (`snappy`), so that the data is stored instead. `formula` is the
//! entry point (see aqr-formula), with the feature `encode`; `module` is the code of the
//! modules, which `FORMAT.md` specifies.
//!
//! LevelDB tables (`leveldb`), whose blocks Snappy compresses, are rebuilt whole: the contents
//! of their blocks are stored, and the trailers and the footer derived.

pub mod crc32c;
#[cfg(feature = "encode")]
pub mod formula;
pub mod leveldb;
pub mod module;
pub mod snappy;

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
