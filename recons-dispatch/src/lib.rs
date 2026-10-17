//! Recompression of a file into an entry formula (xform-design.md §4.2, §6): [`detect`] picks
//! the recompressor by the magic bytes at the start, and [`recompress`] runs it, giving a
//! [`Formula`] rather than bytes.
//!
//! - PE images: exe-recons' x86 disfilter applied to each executable section, over its
//!   streams; the rest of the image is sliced in between.
//! - gzip and zlib: the container sliced around flate-recons on the DEFLATE stream (data and
//!   record), and the data recompressed in turn, up to [`Options::depth`] formats deep.
//! - PNG, JPEG, MP3, FLAC/WAV/AIFF, PSD/PSB, PDF and SQLite: the recompressor taken whole,
//!   applied to its container (see [`native`]).
//!
//! [`compress`] and [`decompress`] store the formula on its own, in the formula container
//! ([`recons_formula::container`], with the module numbers of [`native::IDS`]).
//!
//! Whatever is not recognised, turned down by its recompressor, not made smaller, or (with
//! [`Options::verify`]) not reconstructed by its module is kept raw: a slice of the input.
//! [`Native`] evaluates the formulas.
//!
//! ```no_run
//! # let file: Vec<u8> = vec![];
//! let opt = recons_dispatch::Options::default();
//! let formula = recons_dispatch::recompress(&file, &opt);
//! assert_eq!(formula.eval(&recons_dispatch::Native::default())?, file);
//! # Ok::<(), recons_formula::Error>(())
//! ```

mod detect;
mod flate;
pub mod native;
mod pe;

use std::panic::{AssertUnwindSafe, catch_unwind};

pub use detect::{Format, detect};
pub use native::Native;
pub use recons_formula::{Formula, Module, Node};

use native::Whole;
use recons_formula::{Stream, container};

#[derive(Debug, Clone)]
pub struct Options {
    /// Threads for the recompressors and modules (the formula does not depend on it).
    pub threads: usize,
    /// Formats nested at most this deep: 1 recompresses the file alone, 2 also the data of a
    /// gzip or zlib stream, and so on (0 recompresses nothing).
    pub depth: u32,
    /// Run every module on what it was given, and keep the data raw where the output is not
    /// the original (§6). Without it, the archive must checksum the output (R8).
    pub verify: bool,
}

impl Default for Options {
    fn default() -> Options {
        Options { threads: 1, depth: 3, verify: true }
    }
}

/// `d` as a formula, with the recompressor for its format if any takes it.
pub fn recompress(d: &[u8], opt: &Options) -> Formula {
    recompress_at(d, opt, 0)
}

/// `d` in the formula container. A formula whose description does not fit (over 65535 bytes)
/// is given up for `d` raw.
pub fn compress(d: &[u8], opt: &Options) -> Vec<u8> {
    container::encode(&recompress(d, opt), &native::IDS).unwrap_or_else(|_| {
        container::encode(&Formula::raw("raw", d.to_vec()), &native::IDS).expect("a raw formula fits")
    })
}

/// The file in formula container `c`. Nothing checks the output: that is left to whatever
/// holds the container (R8).
pub fn decompress(c: &[u8], threads: usize) -> recons_formula::Result<Vec<u8>> {
    container::decode(c, &native::IDS, "formula")?.eval(&Native { threads })
}

/// [`recompress`] for data `depth` formats deep.
pub(crate) fn recompress_at(d: &[u8], opt: &Options, depth: u32) -> Formula {
    let format = if depth < opt.depth { detect(d) } else { None };
    format
        .and_then(|format| {
            catch_unwind(AssertUnwindSafe(|| match format {
                Format::Pe => pe::pe(d, opt),
                Format::Gzip => flate::gzip(d, opt, depth),
                Format::Zlib => flate::zlib(d, opt, depth),
                Format::Png => whole(&native::PNG, d, opt),
                Format::Jpeg => whole(&native::JPEG, d, opt),
                Format::Mp3 => whole(&native::MP3, d, opt),
                Format::Flac => whole(&native::FLAC, d, opt),
                Format::Psd => whole(&native::PSD, d, opt),
                Format::Pdf => whole(&native::PDF, d, opt),
                Format::Sqlite => whole(&native::SQLITE, d, opt),
            }))
            .ok()
            .flatten()
        })
        .unwrap_or_else(|| Formula::raw("raw", d.to_vec()))
}

/// The recompressor `w` on `d`, if it makes it smaller (a stored fallback does not).
fn whole(w: &Whole, d: &[u8], opt: &Options) -> Option<Formula> {
    let c = (w.compress)(d, opt.threads);
    let module = w.module();
    if c.len() >= d.len() || !verified(opt, module, &[], &[&c], d) {
        return None;
    }
    let mut f = Formula::default();
    let input = f.push(w.name, c);
    f.root = Node::apply(module, vec![], vec![input], d.len() as u64);
    Some(f)
}

/// Unless verification is off, whether `m` makes `expect` of `inputs`.
pub(crate) fn verified(opt: &Options, m: Module, params: &[u8], inputs: &[&[u8]], expect: &[u8]) -> bool {
    !opt.verify || Native { threads: opt.threads }.run(m, params, inputs).is_ok_and(|out| out == expect)
}

/// Bytes of the streams of `f` by class, largest first.
pub fn classes(f: &Formula) -> Vec<(&str, usize)> {
    let mut out: Vec<(&str, usize)> = Vec::new();
    for Stream { class, data } in &f.streams {
        match out.iter_mut().find(|(c, _)| c == class) {
            Some((_, n)) => *n += data.len(),
            None => out.push((class, data.len())),
        }
    }
    out.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    out
}
