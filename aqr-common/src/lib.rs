//! Building blocks shared by the `aqr-*` recompressors.
//!
//! - [`varint`]: LEB128 and zigzag, for the containers' own streams.
//! - [`rans64`]: alias rANS with static per-context tables, eight interleaved 64-bit states
//!   (aqr-mp3, aqr-flac).
//! - [`rans32`]: rANS with per-file tables stored as quantised logarithms, four interleaved
//!   32-bit states and byte output, with its LSB-first bit I/O (aqr-png, aqr-psd, aqr-pdf).
//! - [`msb`]: MSB-first bit I/O of the formats themselves (MP3, FLAC).
//! - [`mem`]: the global allocator of the binaries ([`mem::Mapped`]), a counting one, peak and
//!   large-allocation probes, peak RSS.
//! - [`rng`]: deterministic pseudo-random numbers and noise, for tests and benchmarks.
//! - [`pool`]: the pool of threads every thread of the process runs on.
//! - [`packed`]: packed byte sequences, a list of them as one.
//! - [`wire`]: values as bytes, for the results of tasks on WebAssembly.
//! - [`crc32`]: CRC-32, without its tables in the modules.
//! - [`arith`]: binary arithmetic coding with adaptive bits (the records of aqr-lzma, aqr-lzo).

pub mod arith;
pub mod crc32;
pub mod mem;
pub mod msb;
pub mod packed;
pub mod pool;
pub mod rans32;
pub mod rans64;
pub mod rng;
pub mod varint;
pub mod wire;

/// Malformed input; `.0` says what was wrong. The crates convert it into their own error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Corrupt(pub &'static str);

impl std::fmt::Display for Corrupt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for Corrupt {}
