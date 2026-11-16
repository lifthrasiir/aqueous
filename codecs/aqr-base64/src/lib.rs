//! Base64 blocks in text files (the attachments of MIME messages, PEM, data URIs, base64 in JSON
//! and XML), cut out of the text and decoded, so that the decoded bytes are recompressed as the
//! files they are (a JPEG, a PNG, HTML) and the text without them as text. The module
//! `base64-recons` ([`module`]) writes them again bit for bit from the text, the decoded bytes
//! and a layout (where each block lies, how its lines are cut). `formula` is the encoder (with
//! the feature `encode`: it finds the blocks); `FORMAT.md` specifies the module.

aqr_common::zopt_namespace!("base64", "base64 blocks in text (aqr-base64)");

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
