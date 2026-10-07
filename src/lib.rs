//! Lossless PNG recompression: a smaller representation from which the original file is
//! rebuilt bit for bit.

pub mod container;
pub mod filter;
pub mod model;
pub mod png;
pub mod rans;

pub use container::{compress, decompress};

#[derive(Debug)]
pub enum Error {
    /// The input is not a PNG file this tool can handle (it is stored instead).
    Unsupported(&'static str),
    /// The compressed data is damaged.
    Corrupt,
    Flate(flate_recons::Error),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Unsupported(m) => write!(f, "unsupported: {m}"),
            Error::Corrupt => f.write_str("corrupt data"),
            Error::Flate(e) => write!(f, "deflate: {e}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<flate_recons::Error> for Error {
    fn from(e: flate_recons::Error) -> Self {
        Error::Flate(e)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// `Error::Corrupt`, reporting where it was raised when `PNGR_DEBUG` is set.
#[track_caller]
#[cold]
pub(crate) fn corrupt() -> Error {
    if std::env::var_os("PNGR_DEBUG").is_some() {
        eprintln!("corrupt at {}", std::panic::Location::caller());
    }
    Error::Corrupt
}
