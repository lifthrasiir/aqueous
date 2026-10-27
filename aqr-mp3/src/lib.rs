//! Lossless MP3 recompression: Layer III files are stored smaller and restored bit for bit.

#![allow(clippy::needless_range_loop)]
// The code of the modules alone uses only part of the rest.
#![cfg_attr(not(feature = "encode"), allow(unused))]

pub mod codec;
#[cfg(feature = "encode")]
pub mod formula;
pub mod frame;
pub mod huff;
pub mod huff_tables;
pub mod model;
pub mod module;
mod par;
pub mod rc;
pub mod side;
pub mod stream;

pub use aqr_common::{msb as bits, rans64 as rans};
