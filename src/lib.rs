//! Lossless PSD/PSB recompression: a Photoshop file is stored smaller and restored bit for
//! bit.
//!
//! ```no_run
//! # let psd: Vec<u8> = vec![];
//! let packed = psd_recons::compress(&psd); // never fails: stores what it cannot code
//! assert_eq!(psd_recons::decompress(&packed)?, psd); // checked against an xxh3 of the original
//! # Ok::<(), psd_recons::Error>(())
//! ```

pub mod composite;
pub mod container;
pub mod model;
pub mod packbits;
pub mod pixels;
pub mod psd;

pub use container::{compress, decompress};
pub use psd::{Error, Psd, Result, parse};
