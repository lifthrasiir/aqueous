//! The module `raster-recons`: a raw raster from its coded samples
//! ([`crate::image::raster`]).
//!
//! Params: the layout ([`Format::to_bytes`]). No resources. Inputs: the coded samples and the
//! row padding.

use super::{Error, Result};
use crate::image::raster::{self, Format};
use aqr_formula::{Formula, Kind, Module, Node};

pub const MODULE: Module = Module { name: "raster-recons", version: 1 };

/// The application giving `samples` (exactly the raster of `fmt`), its inputs pushed to `f`.
/// None where the layout is not taken, or where the coder panics.
pub fn apply(f: &mut Formula, samples: &[u8], fmt: Format) -> Option<Node> {
    let c = std::panic::catch_unwind(|| raster::encode(samples, fmt)).ok()??;
    let inputs = vec![f.push_as("raster/coded", Kind::Compressed, c.coded), f.push("raster/padding", c.pad)];
    Some(Node::apply(MODULE, fmt.to_bytes(), inputs, samples.len() as u64))
}

/// The module: the raster of `len` bytes from `params` (its layout) and `inputs` (the coded
/// samples and the padding).
pub fn decode(params: &[u8], inputs: &[&[u8]], len: u64, threads: usize) -> Result<Vec<u8>> {
    let _ = threads;
    let corrupt = |s: &str| Error(format!("corrupt: {s}"));
    let fmt = Format::from_bytes(params).ok_or_else(|| corrupt("params"))?;
    let &[coded, pad] = inputs else { return Err(corrupt("inputs")) };
    if fmt.len() != Some(len as usize) || usize::try_from(len).is_err() {
        return Err(corrupt("length"));
    }
    let mut out = Vec::new();
    raster::decode(coded, pad, fmt, &mut out).map_err(|e| corrupt(e.0))?;
    Ok(out)
}
