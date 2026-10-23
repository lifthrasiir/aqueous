//! LZ4 recompression: an LZ4 stream is rebuilt bit for bit from its data, by liblz4's fast
//! compressor reimplemented (`block`, `frame`), so that the data is stored instead. `formula`
//! is the entry point (see aqr-formula); `FORMAT.md` specifies the module.
//!
//! Two containers are taken: Firefox's `mozlz4` (one block) and LZ4 frames with independent
//! blocks.

pub mod block;
pub mod formula;
pub mod frame;

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
