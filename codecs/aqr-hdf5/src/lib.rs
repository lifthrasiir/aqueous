//! HDF5 recompression: the deflated chunks of an HDF5 file's datasets are rebuilt bit for bit
//! by aqr-flate from their data, so that the data is stored instead. `formula` is the entry
//! point (see aqr-formula); `hdf5` reads what of a file it needs; `FORMAT.md` tells what is
//! stored.

#[cfg(feature = "encode")]
pub mod formula;
pub mod hdf5;
