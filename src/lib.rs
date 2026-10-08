//! Lossless Wavefront OBJ recompression: an OBJ file is stored smaller, and restored bit for
//! bit.
//!
//! ```
//! let obj = b"v 0.5 1.000000 -2\nf 1 1 1\n".to_vec();
//! let packed = obj_recons::compress(&obj);               // never fails: stores what it cannot code
//! assert_eq!(obj_recons::decompress(&packed).unwrap(), obj);
//! ```

pub mod block;
pub mod container;
pub mod dec;
pub mod mem;
pub mod num;
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
