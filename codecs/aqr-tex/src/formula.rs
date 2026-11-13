//! aqr-tex as modules of entry formulas (see aqr-formula): each group of formats has one
//! (`bcn-recons` of BC1 to BC5, `bc7-recons`, …: [`module`]), which rebuilds the blocks of a
//! texture's layout (its params) from their coded form ([`crate::texture`]). A DDS file is its
//! header and whatever follows its data, as they are, around the application ([`encode`]); a
//! KTX, KTX 2, PVR or `.astc` file the bytes between its surfaces, the application's output cut
//! into them ([`encode_container`]). A DDS file of uncompressed pixels has each of its surfaces
//! applied to aqr-raw's `raster-recons` instead. FORMAT.md describes the modules in full.

use aqr_formula::{Formula, Kind, Module, Node};

use crate::container;
use crate::dds::{self, Data};
use crate::layout::{Group, Layout};
use crate::texture::{self, Family};

pub use crate::module::decode;

pub const MODULE: Module = Module::new("bcn-recons", 1).with_task_plan(memory::<crate::bcn::Bcn>);
pub const MODULE_BC7: Module = Module::new("bc7-recons", 1).with_task_plan(memory::<crate::bc7::Bc7>);
pub const MODULE_BC6H: Module = Module::new("bc6h-recons", 1).with_task_plan(memory::<crate::bc6h::Bc6h>);

/// The module of the formats of group `g`.
pub fn module(g: Group) -> Module {
    match g {
        Group::Bcn => MODULE,
        Group::Bc7 => MODULE_BC7,
        Group::Bc6h => MODULE_BC6H,
        _ => unimplemented!("{g:?}"),
    }
}

/// Stream classes of a DDS file's bytes around its data, and of its surfaces kept as they are.
pub const HEADER: &str = "dds/header";
pub const TRAILER: &str = "dds/trailer";
pub const PIXELS: &str = "dds/pixels";
/// Of the other containers' bytes around their surfaces.
pub const CONTAINER: &str = "tex/container";

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

/// The formula of `input`, a KTX, KTX 2, PVR or `.astc` file ([`container`]), or `None` where
/// it is not one this crate takes: the application of its blocks (of every surface, in the
/// layout's order), its output cut into the surfaces, each where the file has it, and the bytes
/// between them as they are. It is the same whatever `threads` is.
pub fn encode_container(input: &[u8], threads: usize) -> Option<Formula> {
    let found = std::panic::catch_unwind(|| container::detect(input)).ok()??;
    let pieces = found.pieces(input.len())?;
    let blocks: Vec<u8> = pieces.iter().flat_map(|&(s, n)| &input[s..s + n]).copied().collect();
    let mut f = Formula::default();
    let Node::Apply(a) = apply(&mut f, &blocks, &found.layout, threads)? else { unreachable!("an application") };
    let mut outputs: Vec<Option<Node>> = match pieces.len() {
        1 => vec![Some(Node::Apply(a))],
        _ => f.split(*a, pieces.iter().map(|p| p.1 as u64).collect()).into_iter().map(Some).collect(),
    };
    let mut order: Vec<usize> = (0..pieces.len()).collect();
    order.sort_by_key(|&i| pieces[i].0);
    let (mut parts, mut at) = (vec![], 0);
    for i in order {
        let (s, n) = pieces[i];
        if s > at {
            parts.push(f.push(CONTAINER, input[at..s].to_vec()));
        }
        parts.push(outputs[i].take().expect("each piece once"));
        at = s + n;
    }
    if at < input.len() {
        parts.push(f.push(CONTAINER, input[at..].to_vec()));
    }
    f.root = Node::concat(parts);
    Some(f)
}

/// The application giving `blocks`, exactly the blocks of layout `l`, its inputs pushed to `f`.
/// None where its format is not taken, or the coder panics. It is the same whatever `threads`
/// is.
pub fn apply(f: &mut Formula, blocks: &[u8], l: &Layout, threads: usize) -> Option<Node> {
    if !texture::taken(l.format.group()) {
        return None;
    }
    let (main, apart, _) = std::panic::catch_unwind(|| texture::encode_any(l, blocks, threads)).ok()?;
    let inputs =
        vec![f.push_as("bcn/coded", Kind::Compressed, main), f.push_as("bcn/stripes", Kind::Compressed, apart)];
    Some(Node::apply(module(l.format.group()), l.to_bytes(), inputs, blocks.len() as u64))
}

/// The memory of family `F`'s module: its plan ([`crate::module::plan_of`]), its initial memory
/// with it.
fn memory<F: Family>(u: &aqr_formula::Usage<'_>) -> Option<aqr_formula::Plan> {
    let &[len] = u.outputs else { return None };
    crate::module::plan_of::<F>(u.params, u.inputs, u.initial(), len).map(|(i, t)| aqr_formula::Plan::one(i, t))
}

/// Decodes an application of any of the modules (as aqr-decompress would natively), for tests
/// and tools.
pub fn decode_any(a: &aqr_formula::Apply, inputs: &[&[u8]], threads: usize) -> aqr_formula::Result<Vec<u8>> {
    let r = match a.module.name {
        "bcn-recons" => crate::module::decode(&a.params, inputs, a.len, threads),
        "bc7-recons" => crate::module::decode_bc7(&a.params, inputs, a.len, threads),
        "bc6h-recons" => crate::module::decode_bc6h(&a.params, inputs, a.len, threads),
        _ => return Err(aqr_formula::Error::new(format!("{}: not of aqr-tex", a.module.name))),
    };
    r.map_err(|e| aqr_formula::Error::new(e.0))
}
