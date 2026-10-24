//! Uncompressed sampled data: PCM audio ([`audio`]) and raster images ([`image`]).
//!
//! Either way round:
//! - as a library, the models: other recompressors code what their formats decode to with
//!   them (aqr-flac the residuals of FLAC frames, aqr-png the pixels behind DEFLATE's
//!   literals);
//! - as modules of entry formulas ([`formula`]): samples of a given layout are coded, and the
//!   trivial containers around them ([`container`]: WAV, AIFF, BMP, PNM, ...) are taken apart,
//!   their headers and trailers kept in the formula as they are.

#![allow(clippy::needless_range_loop, clippy::len_without_is_empty)]

pub mod audio;
pub mod container;
pub mod formula;
pub mod image;
