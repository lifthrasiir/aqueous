//! aqr-tex as a module of entry formulas (see aqr-formula): `bcn-recons` rebuilds the blocks
//! of a texture's layout (its params) from their coded form ([`crate::texture`]); a DDS file is
//! its header and whatever follows its data, as they are, around the application ([`encode`]).
//! A DDS file of uncompressed pixels has each of its surfaces applied to aqr-raw's
//! `raster-recons` instead. FORMAT.md describes the module in full.

use aqr_formula::{Formula, Kind, Module, Node};

use crate::dds::{self, Data};
use crate::layout::Layout;
use crate::texture;

pub use crate::module::decode;

pub const MODULE: Module = Module { name: "bcn-recons", version: 1 };

/// Stream classes of a DDS file's bytes around its data, and of its surfaces kept as they are.
pub const HEADER: &str = "dds/header";
pub const TRAILER: &str = "dds/trailer";
pub const PIXELS: &str = "dds/pixels";

/// The formula of DDS file `input`, or `None` where it is not one this crate takes ([`dds`]).
/// It is the same whatever `threads` is.
pub fn encode(input: &[u8], threads: usize) -> Option<Formula> {
    let found = std::panic::catch_unwind(|| dds::detect(input)).ok()??;
    let (start, end) = (found.start, found.start + found.data.len() as usize);
    let mut f = Formula::default();
    let mut parts = vec![f.push(HEADER, input[..start].to_vec())];
    match found.data {
        Data::Blocks(l) => parts.push(apply(&mut f, &input[start..end], &l, threads)?),
        Data::Pixels { channels, layout } => {
            let mut at = start;
            for (w, h) in dds::pixel_surfaces(&layout) {
                let n = (w * h) as usize * channels;
                let samples = &input[at..at + n];
                let fmt = aqr_raw::image::raster::Format {
                    width: w as usize,
                    height: h as usize,
                    channels,
                    stride: w as usize * channels,
                };
                // (a surface too small for aqr-raw to code is kept as it is)
                let node = aqr_raw::formula::raster::apply(&mut f, samples, fmt)
                    .unwrap_or_else(|| f.push_as(PIXELS, Kind::Pixels, samples.to_vec()));
                parts.push(node);
                at += n;
            }
        }
    }
    if end < input.len() {
        parts.push(f.push(TRAILER, input[end..].to_vec()));
    }
    f.root = Node::concat(parts);
    Some(f)
}

/// The application giving `blocks`, exactly the blocks of layout `l`, its inputs pushed to `f`.
/// None where the coder panics. It is the same whatever `threads` is.
pub fn apply(f: &mut Formula, blocks: &[u8], l: &Layout, threads: usize) -> Option<Node> {
    let (main, apart, _) = std::panic::catch_unwind(|| texture::encode(l, blocks, threads)).ok()?;
    let inputs =
        vec![f.push_as("bcn/coded", Kind::Compressed, main), f.push_as("bcn/stripes", Kind::Compressed, apart)];
    Some(Node::apply(MODULE, l.to_bytes(), inputs, blocks.len() as u64))
}
