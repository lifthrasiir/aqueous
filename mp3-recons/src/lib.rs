//! Lossless MP3 recompression: Layer III files are stored smaller and restored bit for bit.

#![allow(clippy::needless_range_loop)]

pub mod codec;
pub mod container;
pub mod formula;
pub mod frame;
pub mod huff;
pub mod huff_tables;
pub mod model;
mod par;
pub mod rc;
pub mod side;
pub mod stream;

pub use container::{compress, compress_threads, decompress, decompress_threads};
pub use recons_common::{msb as bits, rans64 as rans};
