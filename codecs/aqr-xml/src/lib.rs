//! XML rebuilt bit for bit from its skeleton and its values in containers by context (after
//! XMill): the text with each element's text and each attribute's value cut out and its
//! structure marked, and the values of each context (an element's text by its parent and name,
//! an attribute's value by its element and name) one after another. The module only takes the
//! text apart and puts it back; each container is text for aqr-compress to recompress in turn.
//! `formula` is the entry point (a module of aqr-formula); `FORMAT.md` specifies it.

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
