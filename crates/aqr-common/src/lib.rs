//! Building blocks shared by the `aqr-*` recompressors.
//!
//! - [`varint`]: LEB128 and zigzag, for the containers' own streams.
//! - [`cursor`]: reading those streams, every read checked.
//! - [`rans64`]: alias rANS with static per-context tables, eight interleaved 64-bit states
//!   (aqr-mp3, aqr-flac).
//! - [`rans32`]: rANS with per-file tables stored as quantised logarithms, four interleaved
//!   32-bit states and byte output, with its LSB-first bit I/O (aqr-png, aqr-psd, aqr-pdf).
//! - [`msb`]: MSB-first bit I/O of the formats themselves (MP3, FLAC).
//! - [`mem`]: the global allocator of the binaries ([`mem::Mapped`]), a counting one, peak and
//!   large-allocation probes, peak RSS.
//! - [`allowance`]: the heap a recompression may take, and its giving up past it.
//! - [`meter`]: the heap of a module's application measured as its instances would take it in
//!   WebAssembly (natively).
//! - [`rng`]: deterministic pseudo-random numbers and noise, for tests and benchmarks.
//! - [`pool`]: the pool of threads every thread of the process runs on.
//! - [`budget`]: the heap the work may take at once (`aqr -M`), and the system's memory.
//! - [`progress`]: how far the work on data is, told by the parts of it done.
//! - [`trace`]: what the threads are doing and the heap by what allocated it, sampled.
//! - [`packed`]: packed byte sequences, a list of them as one.
//! - [`wire`]: values as bytes, for the results of tasks on WebAssembly.
//! - [`crc32`]: CRC-32, without its tables in the modules.
//! - [`adler32`]: Adler-32 (of zlib).
//! - [`arith`]: binary arithmetic coding with adaptive bits (the records of aqr-lzma, aqr-lzo, aqr-bvx, aqr-gif).
//! - [`out`]: output made in order, into a vector or compared with what it should be.
//! - [`streams`]: streams of data and records one after another, and their index.
//! - [`lz`]: what the LZ parsers share (the length of a match).
//! - [`bytes`]: byte slices compared (in SIMD in WebAssembly too).
//! - [`fsio`]: files read and written at offsets (native).
//! - [`text`]: bytes as text (native).
//! - [`wasm`]: WebAssembly modules split into streams and joined back (aqr-wasm, module blocks).

pub mod adler32;
#[cfg(not(target_family = "wasm"))]
pub mod allowance;
#[cfg(target_family = "wasm")]
#[path = "allowance_none.rs"]
pub mod allowance;
pub mod arith;
#[cfg(not(target_family = "wasm"))]
mod blocks;
pub mod budget;
pub mod bytes;
#[cfg(not(target_family = "wasm"))]
pub mod cpu;
pub mod crc32;
pub mod cursor;
#[cfg(not(target_family = "wasm"))]
pub mod fsio;
pub mod lz;
pub mod mem;
#[cfg(not(target_family = "wasm"))]
pub mod meter;
#[cfg(target_family = "wasm")]
#[path = "meter_none.rs"]
pub mod meter;
pub mod msb;
pub mod out;
pub mod packed;
#[cfg(not(target_family = "wasm"))]
pub mod pages;
mod plain;
pub mod pool;
pub mod progress;
pub mod rans32;
pub mod rans64;
pub mod rng;
pub mod streams;
#[cfg(not(target_family = "wasm"))]
pub mod text;
#[cfg(not(target_family = "wasm"))]
pub mod trace;
#[cfg(target_family = "wasm")]
#[path = "trace_none.rs"]
pub mod trace;
pub mod varint;
pub mod wasm;
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
