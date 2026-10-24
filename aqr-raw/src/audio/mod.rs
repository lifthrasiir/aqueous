//! PCM audio: blocks of samples as FLAC-like frames, whose residuals go through an adaptive
//! LMS stage and a static-table rANS model.
//!
//! [`pcm`] codes interleaved samples; [`frame`], [`residual`] and [`model`] are what other
//! coders of audio build on (aqr-flac codes the frames of FLAC files with them, as they are).

pub mod frame;
pub mod model;
pub mod par;
pub mod pcm;
pub mod residual;

pub use aqr_common::{msb as bits, rans64 as rans};
