//! Lines of text (logs, tables of numbers) rebuilt bit for bit from a template for each line and
//! its fields: the words that change (and numbers) cut out of the line, kept column by column,
//! numbers as integers in the way they were printed ([aqr-num](../aqr-num)) or as digit runs of
//! a word. Lines that cannot be so are kept as they are. `formula` is the entry point (a module
//! of aqr-formula); `FORMAT.md` specifies it.

#[cfg(feature = "encode")]
pub mod formula;
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
