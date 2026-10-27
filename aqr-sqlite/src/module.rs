//! The code of the module (see `formula`, and `FORMAT.md`), which needs no aqr-formula, so that
//! aqr-modules builds it into its Wasm module.

use crate::page::Geom;
use crate::transform::{self, NSTREAMS};
use crate::varint::Rd;
use crate::{Result, corrupt};

/// The module: the database of `len` bytes rebuilt from `params` (page size, usable size,
/// schema format) and `inputs` (the streams of the transform, raw).
pub fn decode(params: &[u8], inputs: &[&[u8]], len: u64, threads: usize) -> Result<Vec<u8>> {
    let mut r = Rd::new(params);
    let (psize, usable, fmt4) = (r.usize()?, r.usize()?, r.u8()? != 0);
    if !r.at_end() || inputs.len() != NSTREAMS {
        return Err(corrupt());
    }
    if !(512..=65536).contains(&psize) || !psize.is_power_of_two() || !(480..=psize).contains(&usable) {
        return Err(corrupt());
    }
    let len = usize::try_from(len).map_err(|_| corrupt())?;
    let g = Geom { psize, usable, npages: len / psize, fmt4 };
    transform::decode_threads(&g, len, inputs, threads)
}
