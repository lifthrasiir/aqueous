//! Vorbis I audio packets as the values they code: the setup header ([`setup`]), the decode of
//! a packet against a source of values ([`packet::walk`]), which parses a packet to its values
//! and writes them back bit for bit ([`packet::parse`], [`packet::write`]).

// The code of the module alone uses only part of the rest.
#![cfg_attr(not(feature = "encode"), allow(unused))]

pub mod bits;
pub mod codec;
#[cfg(feature = "encode")]
pub mod formula;
pub mod model;
pub mod module;
pub mod packet;
pub mod setup;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error(pub &'static str);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;
