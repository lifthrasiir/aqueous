//! Lossless SQLite 3 database recompression: a database file is stored smaller, and restored
//! bit for bit.
//!
//! ```
//! # let db = vec![0u8; 100];
//! let packed = sqlite_recons::compress(&db);               // never fails: stores what it cannot code
//! assert_eq!(sqlite_recons::decompress(&packed).unwrap(), db);
//! ```

#![allow(clippy::needless_range_loop)]

pub mod container;
pub mod index;
pub mod page;
pub mod par;
pub mod record;
pub mod transform;
pub mod varint;

pub use container::{compress, decompress};

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

pub use recons_common::mem;
