//! bzip2 recompression: a bzip2 stream is rebuilt bit for bit from the data it holds, by
//! libbzip2's compressor reimplemented (`compress`), so that the data is stored instead.
//! `formula` is the entry point (see aqr-formula), with the feature `encode`; `module` is the
//! code of the module, which `FORMAT.md` specifies. `stream` decodes streams.
//!
//! The Burrows-Wheeler transform is the suffix array (`sais`) of the block written twice, so
//! that rotations sort as libbzip2 sorts them. Only the order of equal rotations (of a
//! periodic block) can differ, which a formula notes.

// The code of the modules alone uses only part of the rest.
#![cfg_attr(not(feature = "encode"), allow(unused))]
pub mod compress;
pub mod crc;
#[cfg(feature = "encode")]
pub mod formula;
pub mod huffman;
pub mod module;
pub mod sais;
pub mod stream;

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

/// The stream header (`BZh` and the level's digit follow).
pub const MAGIC: [u8; 3] = *b"BZh";
/// The 48-bit magic numbers of a block and of the end of a stream.
pub const BLOCK_MAGIC: u64 = 0x3141_5926_5359;
pub const END_MAGIC: u64 = 0x1772_4538_5090;

/// Symbols of RLE1 output in a block of `level`, at most before it is full (libbzip2's
/// `nblockMAX`); a block may run 4 past it.
pub const fn block_max(level: u8) -> usize {
    100_000 * level as usize - 19
}
