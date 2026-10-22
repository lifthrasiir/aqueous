//! Lossless Wavefront OBJ recompression: an OBJ file is split into streams that are stored
//! smaller, and restored bit for bit. `formula` is the entry point (see aqr-formula).

pub mod ans;
pub mod block;
pub mod codec;
mod corner;
pub mod formula;
mod recent;
pub mod style;
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
