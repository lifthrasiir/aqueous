//! aqr-sqlite as a module of entry formulas (see aqr-formula): the page geometry as
//! parameters, the 18 streams of the transform as raw input slots (FORMAT.md, "Formula
//! module").

use crate::page::Geom;
use crate::transform::{self, NSTREAMS, STREAM_NAMES};
use crate::varint::{Rd, put_uv};
use crate::{Result, corrupt};
use aqr_formula::{Formula, Module, Node};

pub const MODULE: Module = Module { name: "sqlite-recons", version: 1 };

#[derive(Clone, Copy, Debug)]
pub struct Options {
    /// Rebuild the file from the streams and compare (no formula on a mismatch).
    pub verify: bool,
    /// No formula if the transform panics (off to find such bugs).
    pub catch: bool,
    /// Threads (1: everything on the calling thread). The streams are the same whatever the
    /// number.
    pub threads: usize,
}

impl Default for Options {
    fn default() -> Self {
        Options { verify: true, catch: true, threads: 1 }
    }
}

/// The formula of `input`, or `None` where the file is not a database that the transform
/// can code.
pub fn encode(input: &[u8], threads: usize) -> Option<Formula> {
    encode_with(input, &Options { threads, ..Default::default() })
}

/// `encode` with `opt`.
pub fn encode_with(input: &[u8], opt: &Options) -> Option<Formula> {
    let enc = if opt.catch {
        std::panic::catch_unwind(|| transform::encode_threads(input, opt.threads)).ok().flatten()
    } else {
        transform::encode_threads(input, opt.threads)
    }?;
    if opt.verify {
        let run = || transform::verify(&enc.geom, input, &enc.streams);
        let ok = if opt.catch {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(run)).unwrap_or(false)
        } else {
            run()
        };
        if !ok {
            return None;
        }
    }
    let mut params = Vec::new();
    put_uv(&mut params, enc.geom.psize as u64);
    put_uv(&mut params, enc.geom.usable as u64);
    params.push(enc.geom.fmt4 as u8);
    let mut f = Formula::default();
    let inputs =
        enc.streams.into_iter().enumerate().map(|(i, s)| f.push(format!("sqlr/{}", STREAM_NAMES[i]), s)).collect();
    f.root = Node::apply(MODULE, params, inputs, input.len() as u64);
    Some(f)
}

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
