//! Block-compressed textures (BC1 to BC5), rebuilt bit for bit from their blocks' endpoints
//! and indices coded by context models ([`model`]): the endpoints predicted from the blocks
//! around, the indices from the pixels around as the blocks decode. The DDS container
//! ([`dds`]) is its header around the blocks, kept as it is. `formula` is the entry point (a
//! module of aqr-formula); `FORMAT.md` specifies it.

// The code of the modules alone uses only part of the rest.
#![cfg_attr(not(feature = "encode"), allow(unused))]
#![allow(clippy::needless_range_loop, clippy::len_without_is_empty)]

pub mod bc;
pub mod coder;
pub mod dds;
#[cfg(feature = "encode")]
pub mod formula;
pub mod layout;
pub mod model;
pub mod module;
pub mod texture;

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
