//! Ogg files (RFC 3533) rebuilt bit for bit from their logical streams' packets and the layout
//! of their pages ([`layout`]): the lacing values follow from the packets, the CRCs from the
//! pages, and the header types, sequence numbers and granule positions are coded against what
//! they are predicted to be. `formula` is the entry point (a module of aqr-formula), which gives
//! Vorbis streams' audio packets to aqr-vorbis; `FORMAT.md` specifies it.

// The code of the module alone uses only part of the rest.
#![cfg_attr(not(feature = "encode"), allow(unused))]

pub mod crc;
#[cfg(feature = "encode")]
pub mod formula;
pub mod layout;
pub mod module;
pub mod page;

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
