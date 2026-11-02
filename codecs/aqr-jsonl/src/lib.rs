//! JSON Lines, rebuilt bit for bit from their values stored by key path: the values of a path
//! (`.actor.login`, `.payload.commits[]`) one after the other, strings as they are written and
//! numbers as integers in the way they were printed ([aqr-num](../aqr-num)). Lines that are not
//! JSON values of the file's style are kept as they are. `formula` is the entry point (a module
//! of aqr-formula); `FORMAT.md` specifies it.

#[cfg(feature = "encode")]
pub mod formula;
pub mod module;
#[cfg(feature = "encode")]
pub mod parse;
pub mod tree;

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
