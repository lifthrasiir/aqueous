//! Recompression of a file into an entry formula (xform-design.md §4.2, §6): [`detect`] picks
//! the recompressor by the magic bytes at the start, and [`recompress`] runs it, giving a
//! [`Formula`] rather than bytes.
//!
//! - PE images: exe-recons' x86 disfilter applied to each executable section, over its
//!   streams; the rest of the image is sliced in between.
//! - gzip and zlib: the container sliced around flate-recons on the DEFLATE stream (data and
//!   record).
//! - PNG, JPEG, MP3, FLAC/WAV/AIFF, PSD/PSB, PDF and SQLite: the recompressor's own formula,
//!   its module applied to its streams, and files it embeds (PDF's DEFLATE streams and images,
//!   PSD's and JPEG's embedded files) applied to theirs (see [`native`]).
//!
//! In whatever formula a recompressor gives, the data that its applications take as it is
//! ([`Options::contents`], by default that of a DEFLATE stream, of gzip, zlib or PDF) is
//! recompressed in turn, up to [`Options::depth`] formats deep, and its formula put in place
//! of the data.
//!
//! Data is taken as one file by the format it starts like ([`Nest::Exact`]), or scanned for
//! files starting anywhere in it ([`Nest::Scan`], the default): each is cut to its end
//! ([`extent`]), and the data between them kept raw, in one stream.
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
mod extent;
mod flate;
pub mod native;
mod pe;

use std::panic::{AssertUnwindSafe, catch_unwind};

pub use detect::{Format, detect};
pub use extent::extent;
pub use native::{Content, Native};
pub use recons_formula::{Formula, Module, Node};

use native::Recons;
use recons_formula::{Stream, container};

#[derive(Debug, Clone)]
pub struct Options {
    /// Threads for the recompressors and modules (the formula does not depend on it).
    pub threads: usize,
    /// Formats nested at most this deep: 1 recompresses the file alone, 2 also the data of a
    /// DEFLATE stream in it (of gzip, zlib or PDF), and so on (0 recompresses nothing).
    pub depth: u32,
    /// Run every module on what it was given, and keep the data raw where the output is not
    /// the original (§6). Without it, the archive must checksum the output (R8).
    pub verify: bool,
    /// The inputs whose data is recompressed in turn, by module and position; by default
    /// [`native::CONTENTS`].
    pub contents: Vec<Content>,
    /// How the file itself is recompressed.
    pub file: Nest,
}

/// How data is recompressed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Nest {
    /// As one file, all of it, of the format it starts like.
    Exact,
    /// As data holding files anywhere: each found is cut to its end, the rest kept raw.
    Scan,
}

impl Default for Options {
    fn default() -> Options {
        Options { threads: 1, depth: 3, verify: true, contents: native::CONTENTS.to_vec(), file: Nest::Scan }
    }
}

/// `d` as a formula, with the recompressor for its format if any takes it.
pub fn recompress(d: &[u8], opt: &Options) -> Formula {
    recompress_at(d, opt, 0, opt.file)
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
pub(crate) fn recompress_at(d: &[u8], opt: &Options, depth: u32, how: Nest) -> Formula {
    let f = match how {
        _ if depth >= opt.depth => None,
        Nest::Exact => detect(d).and_then(|format| recompress_as(format, d, opt, depth)),
        Nest::Scan => scan(d, opt, depth),
    };
    f.unwrap_or_else(|| Formula::raw("raw", d.to_vec()))
}

/// `d` as a file of `format`, with its contents nested, if its recompressor takes it.
fn recompress_as(format: Format, d: &[u8], opt: &Options, depth: u32) -> Option<Formula> {
    let mut f = catch_unwind(AssertUnwindSafe(|| match format {
        Format::Pe => pe::pe(d, opt),
        Format::Gzip => flate::gzip(d, opt),
        Format::Zlib => flate::zlib(d, opt),
        Format::Png => recons(&native::PNG, d, opt),
        Format::Jpeg => recons(&native::JPEG, d, opt),
        Format::Mp3 => recons(&native::MP3, d, opt),
        Format::Flac => recons(&native::FLAC, d, opt),
        Format::Psd => recons(&native::PSD, d, opt),
        Format::Pdf => recons(&native::PDF, d, opt),
        Format::Sqlite => recons(&native::SQLITE, d, opt),
    }))
    .ok()
    .flatten()?;
    nest(&mut f, opt, depth);
    Some(f)
}

/// The files in `d`, if any is found: from the first byte on, a file whose recompressor takes
/// it (up to its [`extent`]) is put in place, and the scan goes on after it; else on from the
/// next byte. Those whose signature is weak (zlib, MPEG audio frames without a tag) are only
/// taken at the start.
fn scan(d: &[u8], opt: &Options, depth: u32) -> Option<Formula> {
    let mut f = Formula::default();
    f.push("raw", Vec::new());
    let (mut gaps, mut parts) = (Vec::new(), Vec::new());
    let (mut at, mut p) = (0, 0);
    while p < d.len() {
        let found = detect::starts(&d[p..], p == 0).and_then(|format| {
            let n = extent(format, &d[p..]).filter(|&n| n > 0)?;
            Some((n, recompress_as(format, &d[p..p + n], opt, depth)?))
        });
        let Some((n, child)) = found else {
            p += 1;
            continue;
        };
        parts.push(Node::slice(0, gaps.len() as u64, (p - at) as u64));
        gaps.extend_from_slice(&d[at..p]);
        parts.push(f.embed(child));
        p += n;
        at = p;
    }
    if parts.is_empty() {
        return None;
    }
    parts.push(Node::slice(0, gaps.len() as u64, (d.len() - at) as u64));
    gaps.extend_from_slice(&d[at..]);
    f.streams[0].data = gaps;
    f.root = Node::concat(parts);
    Some(f)
}

/// Recompresses the contents of the applications in `f` ([`Options::contents`]), a formula of
/// data `depth` formats deep, putting their formulas in place of the slices where they have
/// any application; the bytes left without a slice are dropped.
fn nest(f: &mut Formula, opt: &Options, depth: u32) {
    if depth + 1 >= opt.depth {
        return;
    }
    let mut root = std::mem::replace(&mut f.root, Node::Concat(Vec::new()));
    let nested = nest_below(&mut root, f, opt, depth);
    f.root = root;
    if nested {
        f.compact();
    }
}

/// [`nest`] on node `n` of `f`, whether any content is nested.
fn nest_below(n: &mut Node, f: &mut Formula, opt: &Options, depth: u32) -> bool {
    match n {
        Node::Slice { .. } => false,
        Node::Concat(ns) => ns.iter_mut().fold(false, |any, n| nest_below(n, f, opt, depth) | any),
        Node::Apply(a) => {
            let module = a.module;
            let mut any = false;
            for (i, input) in a.inputs.iter_mut().enumerate() {
                let content = opt.contents.iter().find(|c| c.module == module && c.input == i);
                any |= match (&*input, content) {
                    (&Node::Slice { stream, start, len }, Some(content)) => {
                        let Some(d) =
                            f.streams.get(stream).and_then(|s| s.data.get(start as usize..)?.get(..len as usize))
                        else {
                            continue;
                        };
                        let child = recompress_at(d, opt, depth + 1, content.nest);
                        let some = !child.applies().is_empty();
                        if some {
                            *input = f.embed(child);
                        }
                        some
                    }
                    _ => nest_below(input, f, opt, depth),
                };
            }
            any
        }
    }
}

/// The formula of recompressor `r` on `d`, if it makes one. Whether coding pays off is the
/// recompressor's call (as for its container): the streams are not compressed yet, so their
/// size says little.
fn recons(r: &Recons, d: &[u8], opt: &Options) -> Option<Formula> {
    let f = (r.encode)(d, opt.threads)?;
    if opt.verify && !f.eval(&Native { threads: opt.threads }).is_ok_and(|out| out == d) {
        return None;
    }
    Some(f)
}

/// Unless verification is off, whether `m` makes `expect` of `inputs`.
pub(crate) fn verified(opt: &Options, m: Module, params: &[u8], inputs: &[&[u8]], expect: &[u8]) -> bool {
    !opt.verify
        || Native { threads: opt.threads }.run(m, params, inputs, expect.len() as u64).is_ok_and(|out| out == expect)
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
