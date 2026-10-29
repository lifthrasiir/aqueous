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

pub const MODULE: Module = Module::new("bcn-recons", 1).with_memory(memory);

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

/// The memory of the module, on its 8 tasks (the slots), walking the layers as
/// [`texture::decode`] does.
///
/// The instance holds, besides its inputs (`Usage::base`) and the blocks (the output, made at its
/// size) the models: 6 of them, `Models::new`'s tables of 4-byte bits (the colours' 20692, the
/// values' 34552 each, the alpha's 4624) and two caches of recent endpoints (the keys 64 KiB, the
/// Fenwick tree 64 KiB and the hash table of 32768 buckets, 288 KiB, with the old table as it
/// grows: 150 KiB). From the smallest mipmap of a layer up it holds the planes of the surface
/// before (`pp` bytes a block: 16 a channel, 4 channels in BC2 and BC3, 3 in BC1, 2 in BC5, 1 in
/// BC4). A surface of one stripe adds the planes upsampled from them and its own, the
/// upsampling's rows (u16 a channel; as the planes are, of the rows the surface needs), and its keys
/// and choices (5 bytes a block). A surface of more stripes copies the models, and then each
/// stripe but the first is a task of the instance as it is when it spawns (a copy of the
/// high-water mark of everything before, the layers before included, at most 8 at once) with
/// another copy of the models, the stripe's blocks and the copy that comes back, and what a
/// surface of its size takes; the instance codes the first stripe, then holds all the others'
/// blocks, the planes of the stripes (not for the largest mipmap), and the planes stacked.
fn memory(u: &aqr_formula::Usage<'_>) -> Option<u64> {
    let l = Layout::from_bytes(u.params)?;
    let (bb, pp) = (l.format.block_bytes() as u64, 16 * channels(l.format));
    // (the models: the bits, and the caches of recent endpoints)
    let models = 4 * (20692 + 2 * 34552 + 4624) + 2 * (65536 + 65540 + 294928 + 150000) + 4096;
    let len = u.output();
    let base = u.base() + lg(len) + models;
    let surfaces: Vec<_> = l.mip_surfaces().collect();
    // the instance's high-water mark, and the most of the instance and its tasks
    let (mut hw, mut peak) = (base, base);
    for _ in 0..l.layers.min(2) {
        // the planes of the surface before
        let mut prev = 0u64;
        for (m, sf) in surfaces.iter().enumerate().rev() {
            let blocks = sf.blocks() as u64;
            let n = texture::stripes(sf.blocks());
            // the rows of the planes upsampled from the surface before, and of the stripe
            let before = surfaces.get(m + 1).map_or(0, |s| s.bh as u64 * 4);
            // a stripe of `rows` rows of blocks: the planes upsampled and its own, the
            // upsampling's rows (u16 a channel, at most three) and the keys and choices
            let work = |rows: u64| {
                let px = rows * sf.bw as u64;
                let across = (before.min(rows * 2 + 4) + 2) * sf.bw as u64 * 4 * 2 * 3;
                2 * pp * px + across + 5 * px
            };
            if n == 1 {
                let live = base + prev;
                hw = hw.max(live);
                peak = peak.max(live + work(sf.bh as u64));
                prev = pp * blocks;
                hw = hw.max(base + prev);
                continue;
            }
            let copy = live_with_copy(base + prev, models);
            hw = hw.max(copy);
            let top = texture::rows(0, n, sf.bh).len() as u64;
            let mut tasks: Vec<u64> = (1..n)
                .map(|i| {
                    let rows = texture::rows(i, n, sf.bh).len() as u64;
                    let b = rows * sf.bw as u64 * bb;
                    hw + models + 2 * b + work(rows)
                })
                .collect();
            let here = copy + work(top);
            peak = peak.max(here + top_sum(&mut tasks, 8));
            hw = hw.max(here);
            // the stripes' blocks got and the planes
            let after = copy + (blocks - top * sf.bw as u64) * bb + 2 * pp * blocks;
            peak = peak.max(after);
            hw = hw.max(after);
            prev = pp * blocks;
        }
    }
    Some(peak.max(hw) + 16 * aqr_formula::PAGE)
}

fn live_with_copy(live: u64, models: u64) -> u64 {
    live + models
}

/// The sum of the `k` largest of `v`.
fn top_sum(v: &mut [u64], k: usize) -> u64 {
    v.sort_unstable_by(|a, b| b.cmp(a));
    v.iter().take(k).sum()
}

/// `n` bytes, and a page for the allocator (as it grows the memory for a large block).
fn lg(n: u64) -> u64 {
    n + if n >= aqr_formula::PAGE { aqr_formula::PAGE } else { 0 }
}

/// Channels of the planes of a format: of colours (3), alpha or the first value (1), the second.
fn channels(f: crate::layout::Format) -> u64 {
    use crate::layout::Format::*;
    match f {
        Bc1 => 3,
        Bc2 | Bc3 => 4,
        Bc4 => 1,
        Bc5 => 2,
    }
}
