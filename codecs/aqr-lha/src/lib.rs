//! LHA recompression: archives taken apart into their files' contents (`lha` reads them), the
//! streams of `-lh4-` to `-lh7-` split into their data and a record of their parse and headers
//! (`recons`), so that the data is stored instead. `lzh` decodes and encodes those streams.
//! `FORMAT.md` specifies the record and the module.

#[cfg(feature = "encode")]
pub mod formula;
pub mod huff;
pub mod lha;
pub mod lzh;
pub mod module;
#[cfg(feature = "encode")]
pub mod plan;
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
