//! Building blocks shared by the `*-recons` recompressors.
//!
//! - [`varint`]: LEB128 and zigzag, for the containers' own streams.
//! - [`rans64`]: alias rANS with static per-context tables, eight interleaved 64-bit states
//!   (mp3-recons, flac-recons).
//! - [`rans32`]: rANS with per-file tables stored as quantised logarithms, four interleaved
//!   32-bit states and byte output, with its LSB-first bit I/O (png-, psd-, pdf-recons).
//! - [`msb`]: MSB-first bit I/O of the formats themselves (MP3, FLAC).
//! - [`mem`]: a counting global allocator for memory profiling.
//! - [`rng`]: deterministic pseudo-random numbers and noise, for tests and benchmarks.

pub mod mem;
pub mod msb;
pub mod rans32;
pub mod rans64;
pub mod rng;
pub mod varint;

/// Malformed input; `.0` says what was wrong. The crates convert it into their own error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Corrupt(pub &'static str);

impl std::fmt::Display for Corrupt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for Corrupt {}
