//! GNU gettext message catalogs (`.mo` files), rebuilt bit for bit from the file without its
//! tables: the lengths and offsets of the strings follow from the strings, laid out one after
//! another, and the hash table from the original strings ([`mo`]). `formula` is the entry point
//! (a module of aqr-formula); `FORMAT.md` specifies it.

// The code of the module alone uses only part of the rest.
#![cfg_attr(not(feature = "encode"), allow(unused))]
#[cfg(feature = "encode")]
pub mod formula;
pub mod mo;
pub mod module;

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
