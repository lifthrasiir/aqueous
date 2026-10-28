//! FITS files, rebuilt bit for bit from their parts ([`fits`]): headers as text, integer images
//! coded by a pixel model ([`image`]), tile-compressed images (fpack's `RICE_1`) rebuilt from
//! their pixels and the codes of their Rice blocks ([`rice`], [`tiled`]). `formula` is the entry
//! point (modules of aqr-formula); `FORMAT.md` specifies them.

// The code of the modules alone uses only part of the rest.
#![cfg_attr(not(feature = "encode"), allow(unused))]
pub mod bits;
pub mod fits;
#[cfg(feature = "encode")]
pub mod formula;
pub mod image;
pub mod module;
pub mod rice;
pub mod table;
pub mod tiled;

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
