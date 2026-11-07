//! The module `raster-recons`: a raw raster from its coded samples
//! ([`crate::image::raster`]).
//!
//! Params: the layout ([`Format::to_bytes`]). No resources. Inputs: the coded samples and the
//! row padding.

use crate::image::raster::{self, Format};
use aqr_formula::{Formula, Kind, Module, Node};

pub use crate::module::raster::decode;

pub const MODULE: Module = Module::new("raster-recons", 1).with_plan(memory);

/// The memory of the module: its initial memory, and its plan ([`crate::module::raster::plan`]).
fn memory(u: &aqr_formula::Usage<'_>) -> Option<u64> {
    Some(u.initial() + crate::module::raster::plan(u.params, u.inputs, u.output())?)
}

/// The bytes of a band, about: a raster of more is cut into bands of rows of about this many
/// bytes ([`bands`]), each an application of the module of its own, whose coder starts anew (no
/// row above the first). An application needs about four times its band at most (the inputs
/// imported and copied, the output imported and made): 192 MiB at this length.
pub const BAND: usize = 48 << 20;

/// The rows of each band of a raster of `height` rows of `stride` bytes, in bands of about `band`
/// bytes: equal parts, the last the shorter.
fn bands(height: usize, stride: usize, band: usize) -> Vec<usize> {
    let rows = (band / stride.max(1)).max(1);
    let n = height.div_ceil(rows).max(1);
    let rows = height.div_ceil(n);
    (0..n).map(|i| rows.min(height - i * rows)).collect()
}

/// The application giving `samples` (exactly the raster of `fmt`), its inputs pushed to `f`.
/// None where the layout is not taken, or where the coder panics.
pub fn apply(f: &mut Formula, samples: &[u8], fmt: Format) -> Option<Node> {
    apply_in(f, samples, fmt, 1, BAND)
}

/// [`apply`] in bands of about `band` bytes, `threads` coded at a time. A raster of one band is
/// an application of the two streams; more are applications (a [`Node::concat`]) of slices of the
/// two streams, in which the bands' coded forms follow one another (a stream of its own for each
/// band would be a block of its own in a batch).
pub fn apply_in(f: &mut Formula, samples: &[u8], fmt: Format, threads: usize, band: usize) -> Option<Node> {
    let rows = bands(fmt.height, fmt.stride, band);
    if let [_] = rows[..] {
        let c = std::panic::catch_unwind(|| raster::encode(samples, fmt)).ok()??;
        let inputs = vec![f.push_as("raster/coded", Kind::Compressed, c.coded), f.push("raster/padding", c.pad)];
        return Some(Node::apply(MODULE, fmt.to_bytes(), inputs, samples.len() as u64));
    }
    let bands: Vec<(Format, &[u8])> = rows
        .iter()
        .scan(0, |at, &h| {
            let b = &samples[*at..*at + h * fmt.stride];
            *at += h * fmt.stride;
            Some((Format { height: h, ..fmt }, b))
        })
        .collect();
    let (mut coded, mut pad) = (Vec::new(), Vec::new());
    let mut lens = Vec::with_capacity(bands.len());
    for first in (0..bands.len()).step_by(threads.max(1)) {
        let n = threads.max(1).min(bands.len() - first);
        let done = aqr_common::pool::par_map(n, threads, |i| {
            let (fmt, b) = bands[first + i];
            let c = std::panic::catch_unwind(|| raster::encode(b, fmt)).ok()?;
            aqr_common::progress::done(b);
            c
        });
        for c in done {
            let c = c?;
            lens.push((c.coded.len() as u64, c.pad.len() as u64));
            coded.extend_from_slice(&c.coded);
            pad.extend_from_slice(&c.pad);
        }
    }
    let base = f.streams.len();
    f.push_as("raster/coded", Kind::Compressed, coded);
    f.push("raster/padding", pad);
    let mut at = (0u64, 0u64);
    let nodes: Vec<Node> = bands
        .iter()
        .zip(lens)
        .map(|(&(fmt, b), (c, p))| {
            let inputs = vec![Node::slice(base, at.0, c), Node::slice(base + 1, at.1, p)];
            at = (at.0 + c, at.1 + p);
            Node::apply(MODULE, fmt.to_bytes(), inputs, b.len() as u64)
        })
        .collect();
    Some(Node::concat(nodes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_of_bands() {
        assert_eq!(bands(10, 100, 10_000), [10]);
        assert_eq!(bands(10, 100, 400), [4, 4, 2]);
        assert_eq!(bands(10, 100, 1), [1; 10]);
        assert_eq!(bands(7, 100, 300), [3, 3, 1]);
        assert_eq!(bands(1, 100, 1), [1]);
    }
}
