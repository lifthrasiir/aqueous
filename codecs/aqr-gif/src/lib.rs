//! GIF files whose images' LZW streams are rebuilt bit for bit from their pixels, coded by a
//! context model of palette indices, and a record of the encoder's choices given the pixels.
//! The rest of the file (headers, palettes, extensions) is a skeleton kept as it is.

// The code of the modules alone uses only part of the rest.
#![cfg_attr(not(feature = "encode"), allow(unused))]
#[cfg(feature = "encode")]
pub mod formula;
pub mod gif;
pub mod lzw;
pub mod model;
pub mod module;
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
