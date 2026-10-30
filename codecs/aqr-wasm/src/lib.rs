//! WebAssembly modules, rebuilt bit for bit from streams: the code's instructions split by
//! field, the data segments' bytes apart ([`wasm`]). `formula` is the entry point (a module of
//! aqr-formula); `FORMAT.md` specifies it.

#![cfg_attr(not(feature = "encode"), allow(unused))]
#[cfg(feature = "encode")]
pub mod formula;
pub mod module;
pub mod wasm;

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
