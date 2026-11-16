//! Binary-to-text encodings in text files, cut out of the text and decoded so that the decoded
//! bytes are recompressed as the files they are (a JPEG, a PNG, HTML) and the text without them
//! as text. `b2t-recons` ([`module`], [`formula`]): base64 and hexadecimal blocks (the
//! attachments of MIME messages, PEM, data URIs, base64 in JSON and XML, RTF's pictures, SQL
//! dumps' blobs). `b2t-recons-quopri` ([`quopri`]): quoted-printable bodies of MIME messages.
//! Each writes the text again bit for bit from the text, the decoded bytes and a layout;
//! `formula` is the encoder (with the feature `encode`: it finds the blocks); `FORMAT.md`
//! specifies the modules.

aqr_common::zopt_namespace!("b2t", "base64, hexadecimal and quoted-printable in text (aqr-b2t)");

#[cfg(feature = "encode")]
pub mod formula;
pub mod module;
pub mod quopri;

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
