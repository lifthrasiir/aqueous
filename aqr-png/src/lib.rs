//! Lossless PNG recompression: a smaller representation from which the original file is
//! rebuilt bit for bit.

// The code of the modules alone uses only part of the rest.
#![cfg_attr(not(feature = "encode"), allow(unused))]
pub mod codec;
pub mod filter;
#[cfg(feature = "encode")]
pub mod formula;
pub mod model;
pub mod module;
pub mod png;
pub mod rans;
pub mod similar;

// (for the integration tests' helpers, which unit tests take too)
#[cfg(test)]
extern crate self as aqr_png;

#[derive(Debug)]
pub enum Error {
    /// The input is not a PNG file this tool can handle.
    Unsupported(&'static str),
    /// The coded data is damaged.
    Corrupt,
    Flate(aqr_flate::Error),
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

impl From<aqr_common::Corrupt> for Error {
    #[track_caller]
    fn from(_: aqr_common::Corrupt) -> Self {
        corrupt()
    }
}

impl From<aqr_flate::Error> for Error {
    fn from(e: aqr_flate::Error) -> Self {
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
