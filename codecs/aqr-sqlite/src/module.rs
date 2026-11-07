//! The code of the module (see `formula`, and `FORMAT.md`), which needs no aqr-formula, so that
//! aqr-modules builds it into its Wasm module.

use crate::page::Geom;
use crate::transform::{self, NSTREAMS, Sizes};
use crate::varint::{Rd, put_uv};
use crate::{Result, corrupt};

/// The task slots of the module (aqr-modules' `sqlite-recons`), and so its threads.
pub const SLOTS: usize = 8;

/// Appends the sizes decoding takes ([`Sizes`]) to params: the most bytes of a record, the most
/// columns of a row, then the bytes of each index's keys.
pub fn put_sizes(out: &mut Vec<u8>, z: &Sizes) {
    put_uv(out, z.record);
    put_uv(out, z.cols);
    z.keys.iter().for_each(|&k| put_uv(out, k));
}

/// The geometry and the sizes that `params` give (of a database of `len` bytes).
pub fn params(params: &[u8], len: u64) -> Result<(Geom, Sizes)> {
    let mut r = Rd::new(params);
    let (psize, usable, fmt4) = (r.usize()?, r.usize()?, r.u8()? != 0);
    if !(512..=65536).contains(&psize) || !psize.is_power_of_two() || !(480..=psize).contains(&usable) {
        return Err(corrupt());
    }
    let (record, cols) = (r.uv()?, r.uv()?);
    let mut count = r;
    let mut n = 0;
    while !count.at_end() {
        count.uv()?;
        n += 1;
    }
    let mut keys = Vec::with_capacity(n);
    while !r.at_end() {
        keys.push(r.uv()?);
    }
    let len = usize::try_from(len).map_err(|_| corrupt())?;
    Ok((Geom { psize, usable, npages: len / psize, fmt4 }, Sizes { record, cols, keys }))
}

/// The memory plan of the module ([`aqr_api::stack`]): the limits of its instance and its tasks,
/// its initial memory (`initial` bytes) with it, of params `params`, the streams `s` and
/// a database of `len` bytes (`None` where they are not so): what `aqr_api::whole` copies, the
/// sizes read, and what decoding takes ([`transform::plan`]).
pub fn plan(params: &[u8], s: &[&[u8]], initial: u64, len: u64) -> Option<(u64, u64)> {
    let (g, z) = self::params(params, len).ok()?;
    let lens: Vec<u64> = s.iter().map(|i| i.len() as u64).collect();
    let held = aqr_api::stack::whole(params.len() as u64, &[], &lens) + aqr_api::stack::block(8 * z.keys.len() as u64);
    transform::plan(&g, len, s, &z, initial, held)
}

/// The module: the database of `len` bytes rebuilt from `params` (page size, usable size,
/// schema format, then the sizes decoding takes) and `inputs` (the streams of the transform, raw).
pub fn decode(params: &[u8], inputs: &[&[u8]], len: u64, threads: usize) -> Result<Vec<u8>> {
    let (g, sizes) = self::params(params, len)?;
    if inputs.len() != NSTREAMS {
        return Err(corrupt());
    }
    transform::decode_threads(&g, len as usize, inputs, &sizes, threads)
}
