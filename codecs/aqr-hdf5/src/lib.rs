//! HDF5 recompression: the deflated chunks of an HDF5 file's datasets are rebuilt bit for bit
//! by aqr-flate from their data, so that the data is stored instead. `formula` is the entry
//! point (see aqr-formula); `hdf5` reads what of a file it needs. Datasets of floats are coded
//! by a model of their own ([`float`]), the module `hdf5-recons-float` ([`module`]) giving their
//! chunks to aqr-flate. `FORMAT.md` tells what is stored.

// The code of the module alone uses only part of the rest.
#![cfg_attr(not(feature = "encode"), allow(unused))]

pub mod float;
#[cfg(feature = "encode")]
pub mod formula;
pub mod hdf5;
pub mod module;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error(pub &'static str);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

#[cold]
pub fn corrupt() -> Error {
    Error("corrupt input")
}
