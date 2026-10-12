//! Lossless MP3 recompression: Layer III files are stored smaller and restored bit for bit.

#![allow(clippy::needless_range_loop)]

pub mod codec;
pub mod container;
pub mod frame;
pub mod huff;
pub mod huff_tables;
pub mod model;
pub mod rc;
pub mod side;
pub mod stream;

pub use container::{compress, decompress};
pub use recons_common::{msb as bits, rans64 as rans};
