//! Delimited text tables (CSV, TSV, the `;` and `|` variants, USV), rebuilt bit for bit from
//! their fields stored column by column: text as it is, and numbers as integers in the way
//! they were printed ([aqr-num](../aqr-num)). Records that are not of the table are kept as
//! they are. `formula` is the entry point (a module of aqr-formula); `FORMAT.md` specifies it.

// The code of the modules alone uses only part of the rest.
#![cfg_attr(not(feature = "encode"), allow(unused))]
#[cfg(feature = "encode")]
pub mod formula;
pub mod module;
#[cfg(feature = "encode")]
pub mod parse;
pub mod table;

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
