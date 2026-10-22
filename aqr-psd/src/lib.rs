//! PSD/PSB for the formula module (see aqr-formula): a Photoshop file is coded as a formula of
//! a smaller representation and restored bit for bit.
//!
//! ```no_run
//! # let psd: Vec<u8> = vec![];
//! let formula = aqr_psd::formula::encode(&psd, 1); // None where there is nothing to take out
//! ```

pub mod codec;
pub mod composite;
pub mod formula;
pub mod model;
pub mod packbits;
pub mod pixels;
pub mod psd;
pub mod rans;
pub mod rows;

pub use psd::{Error, Psd, Result, parse};
