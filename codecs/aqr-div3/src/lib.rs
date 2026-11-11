//! DivX 3 / MS-MPEG4 v3 frames, parsed down to their entropy-coded syntax elements and coded again by
//! an adaptive context model (a binary range coder on the paths of the original VLCs). The syntax
//! is walked by one function for both directions (`frame`): when compressing, an element is read
//! from the original bits and coded; when decompressing, it is decoded and its bits written, so
//! that the frame comes back bit for bit. See FORMAT.md.
#![no_std]
extern crate alloc;

mod bits;
mod frame;
mod io;
mod model;
mod rc;
pub mod tables;

pub use frame::{Codec, Error, FrameKind};
pub use rc::{Coder, Decoder, Encoder};
