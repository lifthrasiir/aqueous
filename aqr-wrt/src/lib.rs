//! Text rebuilt bit for bit from its words and markup replaced by codes of one to three bytes,
//! after XWRT (Skibiński's XML word replacing transform): the words and markup (XML tags,
//! character references) that the texts use most, in a dictionary (of a text's own, or shared by
//! the texts archived together), and the text with them replaced, a word taking the space before
//! it and its case in flags ([`words`]). zstd then finds more and shorter matches, and codes a
//! word's code as a literal. `formula` is the entry point (a module of aqr-formula); `FORMAT.md`
//! specifies it.

#[cfg(feature = "encode")]
pub mod formula;
pub mod module;
pub mod words;

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
