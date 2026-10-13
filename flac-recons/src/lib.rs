//! Lossless FLAC recompression: FLAC files are stored smaller and restored bit for bit.
//! WAV, AIFF and headerless PCM are compressed with the same model.

#![allow(clippy::needless_range_loop, clippy::len_without_is_empty)]

pub mod codec;
pub mod container;
pub mod flac;
pub mod model;
pub mod par;
pub mod pcm;
pub mod residual;

pub use container::{Options, compress, compress_opt, decompress, decompress_threads};
pub use recons_common::{msb as bits, rans64 as rans};
