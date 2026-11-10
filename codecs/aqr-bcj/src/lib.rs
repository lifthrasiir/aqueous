//! The filters of xz and 7z: the branch converters (BCJ, `bra`), Delta (`delta`), 7z's BCJ2
//! (`bcj2`) and 7z's Swap2 and Swap4 (`swap`), as modules of entry formulas (`formula`, with the
//! feature `encode`; `module` is the code of the modules) that apply the filters' encoders, so
//! that a filtered stream is rebuilt from the data before the filter. `FORMAT.md` specifies them.
//!
//! `bra`, `delta`, `bcj2` and `swap` are ported from the LZMA SDK (Igor Pavlov, public domain).

pub mod bcj2;
pub mod bra;
pub mod delta;
#[cfg(feature = "encode")]
pub mod formula;
pub mod module;
pub mod swap;

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
