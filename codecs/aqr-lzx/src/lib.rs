//! LZX chunks, as WIM files hold them, split into their data and a record of their parse, and
//! rebuilt bit for bit, so that the data is stored instead. Microsoft's encoder is not
//! reproduced: the record codes its parse given the data. `lzx` reads and writes chunks.

// The code of the modules alone uses only part of the rest.
#![cfg_attr(not(feature = "encode"), allow(unused))]
pub mod lzx;

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
