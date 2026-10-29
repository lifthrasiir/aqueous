//! IMA ADPCM blocks (Xbox ADPCM of FSB5 banks, Microsoft IMA ADPCM of WAV files) as a record of
//! their headers and nibbles under an adaptive model of the decoder's state ([`codec`]), and back
//! bit for bit. `formula` is the entry point (a module of aqr-formula); `FORMAT.md` specifies it.

// The code of the module alone uses only part of the rest.
#![cfg_attr(not(feature = "encode"), allow(unused))]

pub mod codec;
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
