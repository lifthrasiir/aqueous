//! The module `raster-recons-16`: a raw raster of 16-bit samples from its coded samples
//! ([`crate::image::raster16`]).
//!
//! Params: the layout ([`Format::to_bytes`]). No resources. Inputs: the coded samples and the
//! row padding.

use crate::image::raster16::{self, Format};
use aqr_formula::{Formula, Kind, Module, Node};

pub use crate::module::raster16::decode;

pub const MODULE: Module = Module { name: "raster-recons-16", version: 1 };

/// The application giving `samples` (exactly the raster of `fmt`), its inputs pushed to `f`.
/// None where the layout is not taken, or where the coder panics.
pub fn apply(f: &mut Formula, samples: &[u8], fmt: Format) -> Option<Node> {
    let c = std::panic::catch_unwind(|| raster16::encode(samples, fmt)).ok()??;
    let inputs = vec![f.push_as("raster16/coded", Kind::Compressed, c.coded), f.push("raster16/padding", c.pad)];
    Some(Node::apply(MODULE, fmt.to_bytes(), inputs, samples.len() as u64))
}
