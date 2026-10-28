//! The module `raster-recons`: a raw raster from its coded samples
//! ([`crate::image::raster`]).
//!
//! Params: the layout ([`Format::to_bytes`]). No resources. Inputs: the coded samples and the
//! row padding.

use crate::image::raster::{self, Format};
use aqr_formula::{Formula, Kind, Module, Node};

pub use crate::module::raster::decode;

pub const MODULE: Module = Module { name: "raster-recons", version: 1 };

/// The application giving `samples` (exactly the raster of `fmt`), its inputs pushed to `f`.
/// None where the layout is not taken, or where the coder panics.
pub fn apply(f: &mut Formula, samples: &[u8], fmt: Format) -> Option<Node> {
    let c = std::panic::catch_unwind(|| raster::encode(samples, fmt)).ok()??;
    let inputs = vec![f.push_as("raster/coded", Kind::Compressed, c.coded), f.push("raster/padding", c.pad)];
    Some(Node::apply(MODULE, fmt.to_bytes(), inputs, samples.len() as u64))
}
