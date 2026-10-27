//! Lossless FLAC recompression: FLAC files are coded smaller and restored bit for bit.
//!
//! The frames are parsed exactly and their residuals coded with aqr-raw's audio model
//! ([`aqr_raw::audio`]), which also codes WAV, AIFF and headerless PCM.

#![allow(clippy::needless_range_loop, clippy::len_without_is_empty)]
// The code of the modules alone uses only part of the rest.
#![cfg_attr(not(feature = "encode"), allow(unused))]

pub mod codec;
pub mod flac;
#[cfg(feature = "encode")]
pub mod formula;
pub mod module;

pub use aqr_common::{msb as bits, rans64 as rans};
