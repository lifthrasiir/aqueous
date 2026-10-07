//! Lossless MP3 recompression: Layer III files are stored smaller and restored bit for bit.

pub mod bits;
pub mod codec;
pub mod container;
pub mod frame;
pub mod huff;
pub mod huff_tables;
pub mod model;
pub mod rans;
pub mod stream;

pub use container::{compress, decompress};
