//! Lossless FLAC recompression: FLAC files are coded smaller and restored bit for bit.
//! WAV, AIFF and headerless PCM are coded with the same model.

#![allow(clippy::needless_range_loop, clippy::len_without_is_empty)]

pub mod codec;
pub mod flac;
pub mod formula;
pub mod model;
pub mod par;
pub mod pcm;
pub mod residual;

pub use aqr_common::{msb as bits, rans64 as rans};
