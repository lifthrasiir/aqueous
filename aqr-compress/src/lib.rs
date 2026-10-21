//! Recompression of a file into an entry formula (xform-design.md §4.2, §6): [`detect`] picks
//! the recompressor by the magic bytes of a file, and [`recompress`] runs it on each file found,
//! giving a [`Formula`] rather than bytes.
//!
//! - PE images: aqr-exe's x86 disfilter applied to each executable section, over its
//!   streams; the rest of the image is sliced in between.
//! - gzip and zlib: the container sliced around aqr-flate on the DEFLATE stream (data and
//!   record).
//! - PNG, JPEG, MP3, FLAC/WAV/AIFF, PSD/PSB, PDF and SQLite: the recompressor's own formula,
//!   its module applied to its streams, and files it embeds (PDF's DEFLATE streams and images,
//!   PSD's and JPEG's embedded files) applied to theirs (see aqr-decompress for the modules).
//!
//! In whatever formula a recompressor gives, the data that its applications take as it is
//! ([`Options::contents`], by default that of a DEFLATE stream, of gzip, zlib or PDF) is
//! recompressed in turn, up to [`Options::depth`] formats deep, and its formula put in place
//! of the data.
//!
//! Data is taken as one file by the format it starts like ([`Nest::Exact`]), or scanned for
//! files starting anywhere in it ([`Nest::Scan`], the default): each is cut to its end
//! ([`extent`]), and the data between them kept raw, in one stream. Data that recompressors
//! turn down counts against a scan, which gives up past twice the data (and 16 MiB).
//!
//! A formula may be kept within [`Options::description`] bytes of description: of the files
//! found, or nested, those with the fewest bytes per byte of their description are kept raw
//! where all would not fit.
//!
//! [`compress`] stores the formula as an Aqueous archive of the file alone
//! ([`aqr_formula::aqr`]), with the modules of [`aqr_decompress::MODULES`], which
//! [`aqr_decompress::decompress`] decompresses.
//!
//! Whatever is not recognised, turned down by its recompressor, not made smaller, or (with
//! [`Options::verify`]) not reconstructed by its module is kept raw: a slice of the input. So
//! are, with [`Options::smaller`], a file and the files found in data whose formula would
//! archive no smaller than they do raw.
//! [`Native`](aqr_decompress::Native) evaluates the formulas.
//!
//! ```no_run
//! # let file: Vec<u8> = vec![];
//! let opt = aqr_compress::Options::default();
//! let formula = aqr_compress::recompress(&file, &opt);
//! assert_eq!(formula.eval(&aqr_decompress::Native::default())?, file);
//! # Ok::<(), aqr_formula::Error>(())
//! ```

mod detect;
mod extent;
mod flate;
mod pe;

use std::panic::{AssertUnwindSafe, catch_unwind};

pub use aqr_formula::{Formula, Module, Node};
pub use detect::{Format, detect};
pub use extent::extent;

use aqr_decompress::{FLATE, Native};
use aqr_formula::Stream;
use aqr_formula::aqr::{self, Encoder, Level, description_bound};

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
    /// [`CONTENTS`].
    pub contents: Vec<Content>,
    /// How the file itself is recompressed.
    pub file: Nest,
    /// The most bytes of description a formula may take ([`aqr::description_bound`]): where
    /// the files found in data, or the contents of a file, would take more, those with the
    /// fewest bytes per byte of their description are kept raw. By default no limit.
    pub description: usize,
    /// The level of the archive of [`compress`] ([`aqr::Encoder`]), and of the formulas sized
    /// for [`Options::smaller`].
    pub level: Level,
    /// Keep a file raw, or the data where files are found, unless its formula archives
    /// smaller than it does raw ([`aqr::Encoder::smaller`]).
    pub smaller: bool,
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
        Options {
            threads: 1,
            depth: 3,
            verify: true,
            contents: CONTENTS.to_vec(),
            file: Nest::Scan,
            description: usize::MAX,
            level: Level::Mid,
            smaller: true,
        }
    }
}

/// An input of a module's applications that is data as it is, a file or what a file holds,
/// rather than a representation of the module's own. A formula may recompress it in turn (see
/// [`Options::contents`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Content {
    pub module: Module,
    pub input: usize,
    pub nest: Nest,
}

/// The contents of the modules of aqr-decompress: the data of [`FLATE`], scanned.
pub const CONTENTS: [Content; 1] = [Content { module: FLATE, input: 0, nest: Nest::Scan }];

/// `d` as a formula, with the recompressor for its format if any takes it.
pub fn recompress(d: &[u8], opt: &Options) -> Formula {
    recompress_at(d, opt, 0, opt.file, &Encoder::new(opt.level))
}

/// `d` as an archive, with the checksum of `d`.
pub fn compress(d: &[u8], opt: &Options) -> Vec<u8> {
    let enc = Encoder::new(opt.level);
    let f = recompress_at(d, opt, 0, opt.file, &enc);
    enc.encode(&f, Some(aqr::checksum(d))).expect("slices within their streams")
}

/// [`recompress`] for data `depth` formats deep, sizing formulas by `enc`.
pub(crate) fn recompress_at(d: &[u8], opt: &Options, depth: u32, how: Nest, enc: &Encoder) -> Formula {
    let f = match how {
        _ if depth >= opt.depth => None,
        Nest::Exact => detect(d).and_then(|format| recompress_as(format, d, opt, depth, enc)),
        Nest::Scan => scan(d, opt, depth, enc).filter(|f| smaller(f, d, opt, enc)),
    };
    f.unwrap_or_else(|| Formula::raw("raw", d.to_vec()))
}

/// `d` as a file of `format`, with its contents nested, if its recompressor takes it (and the
/// formula is [`smaller`]).
fn recompress_as(format: Format, d: &[u8], opt: &Options, depth: u32, enc: &Encoder) -> Option<Formula> {
    let mut f = catch_unwind(AssertUnwindSafe(|| match format {
        Format::Pe => pe::pe(d, opt),
        Format::Gzip => flate::gzip(d, opt),
        Format::Zlib => flate::zlib(d, opt),
        Format::Png => recons(aqr_png::formula::encode, d, opt),
        Format::Jpeg => recons(aqr_jpeg::formula::encode, d, opt),
        Format::Mp3 => recons(aqr_mp3::formula::encode, d, opt),
        Format::Flac => recons(aqr_flac::formula::encode, d, opt),
        Format::Psd => recons(aqr_psd::formula::encode, d, opt),
        Format::Pdf => recons(aqr_pdf::formula::encode, d, opt),
        Format::Sqlite => recons(aqr_sqlite::formula::encode, d, opt),
    }))
    .ok()
    .flatten()?;
    nest(&mut f, opt, depth, enc);
    smaller(&f, d, opt, enc).then_some(f)
}

/// Unless [`Options::smaller`] is off, whether formula `f` of `d` archives smaller than `d`
/// raw, by `enc`.
fn smaller(f: &Formula, d: &[u8], opt: &Options, enc: &Encoder) -> bool {
    !opt.smaller || enc.smaller(f, d).expect("slices within their streams")
}

/// Bytes of the data that a scan may give to recompressors which turn it down, on top of
/// twice the data: past them, no more files are looked for.
const SCAN_SLACK: usize = 16 << 20;

/// The files in `d`, if any is found: from the first byte on, a file whose recompressor takes
/// it (up to its [`extent`]) is put in place, and the scan goes on after it; else on from the
/// next byte, so that files in one turned down are still found. Weak signatures are confirmed
/// by the extent: a zlib stream by its checksum, MPEG audio frames without a tag by one
/// following another.
fn scan(d: &[u8], opt: &Options, depth: u32, enc: &Encoder) -> Option<Formula> {
    scan_with(d, opt, depth, SCAN_SLACK, enc)
}

/// [`scan`] with `slack` for [`SCAN_SLACK`].
fn scan_with(d: &[u8], opt: &Options, depth: u32, slack: usize, enc: &Encoder) -> Option<Formula> {
    let mut found = Vec::new();
    let (mut p, mut budget) = (0, d.len().saturating_mul(2).saturating_add(slack));
    while p < d.len() {
        let child = detect::starts(&d[p..]).filter(|_| budget > 0).and_then(|format| {
            let n = extent(format, &d[p..]).filter(|&n| n > 0)?;
            let child = recompress_as(format, &d[p..p + n], opt, depth, enc);
            if child.is_none() {
                budget = budget.saturating_sub(n);
            }
            Some((n, child?))
        });
        match child {
            Some((n, child)) => {
                found.push((p, n, child));
                p += n;
            }
            None => p += 1,
        }
    }
    // each file with the data before it, and the data after the last and the whole
    let around = description_bound(&Node::slice(0, u64::MAX, d.len() as u64));
    let sizes: Vec<_> =
        found.iter().map(|(_, n, child)| (*n, description_bound(&child.root).saturating_add(around))).collect();
    let keep = fit(around + 11, &sizes, opt.description);

    let mut f = Formula::default();
    f.push("raw", Vec::new());
    let (mut gaps, mut parts, mut at) = (Vec::new(), Vec::new(), 0);
    for ((p, n, child), keep) in found.into_iter().zip(keep) {
        if keep {
            parts.push(Node::slice(0, gaps.len() as u64, (p - at) as u64));
            gaps.extend_from_slice(&d[at..p]);
            parts.push(f.embed(child));
            at = p + n;
        }
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

/// Which of `parts` (bytes of data and of description each) to keep, so that `base` and the
/// descriptions of those kept take at most `limit`: those with the fewest bytes per byte of
/// description go first.
fn fit(base: usize, parts: &[(usize, usize)], limit: usize) -> Vec<bool> {
    let mut keep = vec![true; parts.len()];
    let mut total = parts.iter().fold(base as u128, |t, p| t + p.1 as u128);
    let mut order: Vec<usize> = (0..parts.len()).collect();
    // by n / desc, as n * desc' < n' * desc
    order.sort_by_key(|&i| std::cmp::Reverse(parts[i].1 as u128 * u64::MAX as u128 / (parts[i].0 as u128 + 1)));
    for i in order {
        if total <= limit as u128 {
            break;
        }
        keep[i] = false;
        total -= parts[i].1 as u128;
    }
    keep
}

/// Recompresses the contents of the applications in `f` ([`Options::contents`]), a formula of
/// data `depth` formats deep, putting their formulas in place of the slices where they have
/// any application (as many as fit in [`Options::description`]); the bytes left without a
/// slice are dropped.
fn nest(f: &mut Formula, opt: &Options, depth: u32, enc: &Encoder) {
    if depth + 1 >= opt.depth {
        return;
    }
    let mut children = Vec::new();
    contents(&f.root, f, opt, depth, enc, &mut children);
    let sizes: Vec<_> = children
        .iter()
        .map(|(slice, child)| match child {
            Some(c) => (slice.len() as usize, description_bound(&c.root).saturating_sub(description_bound(slice))),
            None => (0, 0),
        })
        .collect();
    let keep = fit(description_bound(&f.root), &sizes, opt.description);
    if !children.iter().zip(&keep).any(|((_, child), &keep)| keep && child.is_some()) {
        return;
    }
    let mut children = children.into_iter().zip(keep).map(|((_, child), keep)| child.filter(|_| keep));
    let mut root = std::mem::replace(&mut f.root, Node::Concat(Vec::new()));
    place(&mut root, f, opt, &mut children);
    f.root = root;
    f.compact();
}

/// The contents below node `n` of `f`, in order: each slice, and its formula where it has any
/// application.
fn contents(n: &Node, f: &Formula, opt: &Options, depth: u32, enc: &Encoder, out: &mut Vec<(Node, Option<Formula>)>) {
    match n {
        Node::Slice { .. } => {}
        Node::Concat(ns) => ns.iter().for_each(|n| contents(n, f, opt, depth, enc, out)),
        Node::Apply(a) => {
            for (i, input) in a.inputs.iter().enumerate() {
                match (input, content(opt, a.module, i)) {
                    (&Node::Slice { stream, start, len }, Some(content)) => {
                        let d = f.streams.get(stream).and_then(|s| s.data.get(start as usize..)?.get(..len as usize));
                        let child = d.map(|d| recompress_at(d, opt, depth + 1, content.nest, enc));
                        out.push((input.clone(), child.filter(|c| !c.applies().is_empty())));
                    }
                    _ => contents(input, f, opt, depth, enc, out),
                }
            }
        }
    }
}

/// Puts the formulas of the contents below node `n` of `f`, in the order of [`contents`], in
/// place of their slices.
fn place(n: &mut Node, f: &mut Formula, opt: &Options, children: &mut impl Iterator<Item = Option<Formula>>) {
    match n {
        Node::Slice { .. } => {}
        Node::Concat(ns) => ns.iter_mut().for_each(|n| place(n, f, opt, children)),
        Node::Apply(a) => {
            let module = a.module;
            for (i, input) in a.inputs.iter_mut().enumerate() {
                match (&*input, content(opt, module, i)) {
                    (Node::Slice { .. }, Some(_)) => {
                        if let Some(child) = children.next().flatten() {
                            *input = f.embed(child);
                        }
                    }
                    _ => place(input, f, opt, children),
                }
            }
        }
    }
}

/// The content that input `i` of `m` is, if any.
fn content(opt: &Options, m: Module, i: usize) -> Option<&Content> {
    opt.contents.iter().find(|c| c.module == m && c.input == i)
}

/// The formula that a recompressor makes of `d` by `encode` (its `formula` module, see its
/// FORMAT.md), if it makes one. Whether coding pays off is the
/// recompressor's call (as for its container): the streams are not compressed yet, so their
/// size says little.
fn recons(encode: fn(&[u8], usize) -> Option<Formula>, d: &[u8], opt: &Options) -> Option<Formula> {
    let f = encode(d, opt.threads)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Data given to recompressors that turn it down counts against a scan, which stops
    /// looking for files past twice the data.
    #[test]
    fn scan_budget() {
        let jpeg = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../aqr-jpeg/tests/data/gradient.jpg")).unwrap();
        // PNG signatures with a chunk running to the end of the data, each turned down
        let fake = [&b"\x89PNG\r\n\x1a\n"[..], &0x7fff_0000u32.to_be_bytes(), b"IHDR"].concat();
        let data = |fakes: usize| [fake.repeat(fakes), vec![0; 10000], jpeg.clone()].concat();
        let found = |d: &[u8]| {
            scan_with(d, &Options::default(), 0, 0, &Encoder::new(Level::Mid)).map_or(0, |f| f.applies().len())
        };
        assert_eq!(found(&data(1)), 1);
        assert_eq!(found(&data(3)), 0);
    }
}
