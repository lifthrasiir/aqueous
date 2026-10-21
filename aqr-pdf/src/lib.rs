//! Lossless PDF recompression: a PDF file is stored smaller, and restored bit for bit.

pub mod container;
pub mod flate;
pub mod formula;
pub mod image;
pub mod kind;
pub mod lex;
pub mod objects;
pub mod rans;
pub mod scan;
pub mod tok;
pub mod varint;

pub use container::{compress, decompress};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    Corrupt,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Error::Corrupt => write!(f, "corrupt or unsupported input"),
        }
    }
}

impl std::error::Error for Error {}

impl From<aqr_common::Corrupt> for Error {
    fn from(_: aqr_common::Corrupt) -> Self {
        Error::Corrupt
    }
}

pub type Result<T> = std::result::Result<T, Error>;
