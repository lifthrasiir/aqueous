//! LZNT1 (NTFS's compression of files and `RtlCompressBuffer`'s buffers) split into its data and
//! a record of its parse, and rebuilt bit for bit, so that the data is stored instead. Microsoft's
//! encoder is not reproduced: the record codes its parse given the data (`recons`). `lznt1`
//! reads and writes chunks and units; `formula` is the entry point (see aqr-formula), with the
//! feature `encode`; `module` is the code of the module, which `FORMAT.md` specifies.

#[cfg(feature = "encode")]
pub mod formula;
pub mod lznt1;
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
