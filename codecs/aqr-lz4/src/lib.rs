//! LZ4 recompression: an LZ4 stream is rebuilt bit for bit from its data, by liblz4's fast
//! compressor reimplemented (`block`, `frame`), so that the data is stored instead. `formula`
//! is the entry point (see aqr-formula), with the feature `encode`; `module` is the code of the
//! module, which `FORMAT.md` specifies.
//!
//! Two containers are taken: Firefox's `mozlz4` (one block) and LZ4 frames with independent
//! blocks.

pub mod block;
#[cfg(feature = "encode")]
pub mod formula;
pub mod frame;
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
