//! The module `raster-recons`: a raw raster from its coded samples
//! ([`crate::image::raster`]).
//!
//! Params: the layout ([`Format::to_bytes`]). No resources. Inputs: the coded samples and the
//! row padding.

use super::banded::{self, Layout};
use crate::image::raster::{self, Format};
use aqr_formula::{Formula, Module, Node};

pub use crate::module::raster::decode;

pub const MODULE: Module = Module::new("raster-recons", 1).with_plan(memory);

/// The memory of the module: its initial memory, and its plan ([`crate::module::raster::plan`]).
fn memory(u: &aqr_formula::Usage<'_>) -> Option<u64> {
    Some(u.initial() + crate::module::raster::plan(u.params, u.inputs, u.output())?)
}

/// The bytes of a band, about: a raster of more is cut into bands of rows of about this many
/// bytes (`bands`), each an application of the module of its own, whose coder starts anew (no
/// row above the first). An application needs about four times its band at most (the inputs
/// imported and copied, the output imported and made): 192 MiB at this length.
pub const BAND: usize = 48 << 20;

impl Layout for Format {
    const MODULE: Module = MODULE;
    const CODED: &'static str = "raster/coded";
    const PADDING: &'static str = "raster/padding";

    fn height(&self) -> usize {
        self.height
    }

    fn stride(&self) -> usize {
        self.stride
    }

    fn with_height(self, height: usize) -> Self {
        Format { height, ..self }
    }

    fn to_bytes(&self) -> Vec<u8> {
        Format::to_bytes(self)
    }

    fn encode(d: &[u8], f: Self) -> Option<(Vec<u8>, Vec<u8>)> {
        let c = raster::encode(d, f)?;
        Some((c.coded, c.pad))
    }
}

/// The application giving `samples` (exactly the raster of `fmt`), its inputs pushed to `f`.
/// None where the layout is not taken.
pub fn apply(f: &mut Formula, samples: &[u8], fmt: Format) -> Option<Node> {
    apply_in(f, samples, fmt, 1, BAND)
}

/// [`apply`] in bands of about `band` bytes, `threads` coded at a time. A raster of one band is
/// an application of the two streams; more are applications (a [`Node::concat`]) of slices of the
/// two streams, in which the bands' coded forms follow one another (a stream of its own for each
/// band would be a block of its own in a batch).
pub fn apply_in(f: &mut Formula, samples: &[u8], fmt: Format, threads: usize, band: usize) -> Option<Node> {
    banded::apply_in(f, samples, fmt, threads, band)
}
