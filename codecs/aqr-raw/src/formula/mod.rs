//! aqr-raw as modules of entry formulas (see aqr-formula): `pcm-recons` ([`pcm`]),
//! `raster-recons` ([`raster`]) and `raster-recons-16` ([`raster16`]) each give the samples of a layout, their params, from their
//! coded streams. A container is its header and trailer, as they are, around the
//! application ([`encode`]); IMA ADPCM blocks (of WAV) are aqr-ima's `ima-recons`'. A recompressor that finds samples in a format of its own puts
//! them into its formula the same way ([`apply`]). FORMAT.md describes the modules in full.

pub mod pcm;
pub mod raster;
pub mod raster16;

use crate::container::{self, Found, Layout};
use aqr_formula::{Formula, Node};

pub use crate::module::{Error, Result};

/// Stream classes of a container's bytes around its samples.
pub const HEADER: &str = "raw/header";
pub const TRAILER: &str = "raw/trailer";

/// The formula of `input`, a container that [`container::detect`] takes apart. None where it
/// is not one, or where its samples are not coded.
pub fn encode(input: &[u8], threads: usize) -> Option<Formula> {
    let found = std::panic::catch_unwind(|| container::detect(input)).ok()??;
    encode_found(input, &found, threads)
}

/// The formula of `input`, data of no format, as 16-bit samples after a header that tells their
/// size ([`container::sized`]). None where it is not, or where its samples are not coded.
pub fn encode_sized(input: &[u8], threads: usize) -> Option<Formula> {
    let found = std::panic::catch_unwind(|| container::sized(input)).ok()??;
    encode_found(input, &found, threads)
}

/// The formula of `input` whose samples are where `found` says (a container parsed
/// elsewhere): the bytes before and after them as they are, around the samples' application.
pub fn encode_found(input: &[u8], found: &Found, threads: usize) -> Option<Formula> {
    let (start, end) = (found.start, found.start.checked_add(found.len)?);
    let samples = input.get(start..end)?;
    let mut f = Formula::default();
    let node = apply(&mut f, samples, found.layout, threads)?;
    let mut parts = Vec::new();
    if start > 0 {
        parts.push(f.push(HEADER, input[..start].to_vec()));
    }
    parts.push(node);
    if end < input.len() {
        parts.push(f.push(TRAILER, input[end..].to_vec()));
    }
    f.root = Node::concat(parts);
    Some(f)
}

/// The formula of headerless samples of `layout` (all of `input` for PCM, but for a partial
/// sample frame at the end; the raster from the start for an image), the rest kept as a
/// trailer.
pub fn encode_samples(input: &[u8], layout: Layout, threads: usize) -> Option<Formula> {
    let len = match layout {
        Layout::Pcm(f) if f.valid() => input.len() - input.len() % f.frame_bytes(),
        Layout::Raster(f) => f.len().filter(|&l| l <= input.len())?,
        Layout::Raster16(f) => f.len().filter(|&l| l <= input.len())?,
        Layout::Ima(f) if f.valid() => input.len() - input.len() % f.block(),
        _ => return None,
    };
    encode_found(input, &Found { name: "raw", layout, start: 0, len }, threads)
}

/// The application giving `samples` of `layout`, its inputs pushed to `f` as streams. None
/// where they are not coded (nothing to code, a layout not taken, a panic of the coder).
pub fn apply(f: &mut Formula, samples: &[u8], layout: Layout, threads: usize) -> Option<Node> {
    match layout {
        Layout::Pcm(fmt) => pcm::apply(f, samples, fmt, threads),
        Layout::Raster(fmt) => raster::apply(f, samples, fmt),
        Layout::Raster16(fmt) => raster16::apply(f, samples, fmt),
        Layout::Ima(fmt) => {
            let l = aqr_ima::codec::Layout::new(fmt.channels, fmt.per_channel)?;
            // Any bytes of whole blocks are coded (aqr-ima's record keeps them all).
            let (node, used) = aqr_ima::formula::blocks(f, l, samples)?;
            (used == samples.len()).then_some(node)
        }
    }
}
