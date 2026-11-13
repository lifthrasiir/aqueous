//! Microsoft ADPCM blocks (of WAV files, `WAVE_FORMAT_ADPCM`, and of XACT's wave banks) as a
//! record of their headers and nibbles under an adaptive model of the decoder's state
//! ([`codec`]), and back bit for bit. `formula` is the entry point (a module of aqr-formula);
//! `FORMAT.md` specifies it.

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
