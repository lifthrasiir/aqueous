//! DivX 3 / MS-MPEG4 v3 frames, parsed down to their entropy-coded syntax elements and coded again by
//! an adaptive context model (a binary range coder on the paths of the original VLCs). The syntax
//! is walked by one function for both directions (`frame`): when compressing, an element is read
//! from the original bits and coded; when decompressing, it is decoded and its bits written, so
//! that the frame comes back bit for bit. `module` is the code of the module `div3-recons`; `formula`
//! (feature `encode`) makes the formulas of AVI files that use it.
#![no_std]
extern crate alloc;
#[cfg(feature = "encode")]
extern crate std;

mod frame;
pub mod tables;

#[cfg(feature = "encode")]
pub mod formula;
pub mod module;
/// The module natively on whole inputs (not in the module: what aqr-decompress and aqr-dll run).
#[cfg(not(target_family = "wasm"))]
pub mod native;

pub use frame::{Codec, FrameError, FrameKind};

pub use aqr_walk::rc::{Coder, Decoder, Encoder, SliceSource, Source};

/// The error of the module and the formula.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error(pub &'static str);

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.0)
    }
}

#[cfg(feature = "encode")]
impl std::error::Error for Error {}

pub type Result<T> = core::result::Result<T, Error>;

#[cold]
pub fn corrupt() -> Error {
    Error("corrupt input")
}
