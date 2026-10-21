//! Lossless PSD/PSB recompression: a Photoshop file is stored smaller and restored bit for
//! bit.
//!
//! ```no_run
//! # let psd: Vec<u8> = vec![];
//! let packed = aqr_psd::compress(&psd); // never fails: stores what it cannot code
//! assert_eq!(aqr_psd::decompress(&packed)?, psd); // checked against an xxh3 of the original
//! # Ok::<(), aqr_psd::Error>(())
//! ```

pub mod composite;
pub mod container;
pub mod formula;
pub mod model;
pub mod packbits;
pub mod pixels;
pub mod psd;
pub mod rans;
pub mod rows;

pub use container::{Options, compress, compress_with, decompress, decompress_threads};
pub use psd::{Error, Psd, Result, parse};
