//! Recompression of a file into an entry formula (xform-design.md §4.2, §6): [`detect`] picks
//! the recompressor by the magic bytes of a file, and [`recompress`] runs it on each file found,
//! giving a [`Formula`] rather than bytes.
//!
//! - PE images, and Mach-O images (thin or universal): aqr-exe's x86 disfilter or ARM64
//!   filter applied to each executable section, over its streams; the rest of the file is
//!   sliced in between.
//! - gzip and zlib: the container sliced around aqr-flate on the DEFLATE stream (data and
//!   record).
//! - mozlz4 files and LZ4 frames: the container sliced around aqr-lz4 on the data.
//! - LevelDB tables: aqr-snappy on the contents of their blocks.
//! - ZIP archives: aqr-zip, rebuilding the archive from the contents of its entries (the
//!   DEFLATE streams by aqr-flate) with its headers derived.
//! - PNG, JPEG, MP3, FLAC/WAV/AIFF, PSD/PSB, PDF and SQLite: the recompressor's own formula,
//!   its module applied to its streams, and files it embeds (PDF's DEFLATE streams and images,
//!   PSD's and JPEG's embedded files) applied to theirs (see aqr-decompress for the modules).
//!
//! In whatever formula a recompressor gives, the data that its applications take as it is
//! ([`Options::contents`], by default that of a DEFLATE stream, of gzip, zlib or PDF, and
//! that of an LZ4 stream, of the blocks of a LevelDB table or of the entries of a ZIP archive)
//! is recompressed in turn, up to [`Options::depth`] formats deep, and its formula put in place
//! of the data.
//!
//! Data is taken as one file by the format it starts like ([`Nest::Exact`]), or scanned for
//! files starting anywhere in it ([`Nest::Scan`], the default): each is cut to its end
//! ([`extent`]), and the data between them kept raw, in one stream. Data that recompressors
//! turn down counts against a scan, which gives up past twice the data (and 16 MiB).
//!
//! What DEFLATE streams inflate to, at every depth together, is kept within
//! [`Options::expansion`] times the file (and 64 MiB): past it, streams are kept as they are,
//! so that nested archives (42.zip) and bombs cost no more than that.
//!
//! A formula may be kept within [`Options::description`] bytes of description: of the files
//! found, or nested, those with the fewest bytes per byte of their description are kept raw
//! where all would not fit.
//!
//! [`compress`] stores the formula as an Aqueous archive of the file alone
//! ([`aqr_formula::aqr`]), with the modules of [`aqr_decompress::MODULES`], which
//! [`aqr_decompress::decompress`] decompresses.
//!
//! Whatever is not recognised, turned down by its recompressor or not made smaller is kept raw:
//! a slice of the input. So are, with [`Options::smaller`], a file and the files found in data
//! whose formula would archive no smaller than they do raw.
//!
//! Formulas are verified by whoever takes them, not as they are made: with
//! [`Options::verify`], [`compress`] decodes the archive it makes, and stores the file raw
//! instead where that does not give it back. [`recompress`] does not verify. The applications
//! of modules are evaluated beforehand, while the file is still recompressed, and their outputs
//! taken by the decoding (`verify`).
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
mod exe;
mod extent;
mod flate;
mod verify;
mod zip;

use std::cell::Cell;
use std::ops::Range;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

pub use aqr_formula::{Formula, Module, Node};
pub use detect::{Format, detect};
pub use extent::extent;

use aqr_decompress::{FLATE, LZ4, Native, SNAPPY, ZIP};
use aqr_formula::Stream;
use aqr_formula::aqr::{self, Encoder, Level, description_bound};

#[derive(Debug, Clone)]
pub struct Options {
    /// Threads for the recompressors and modules (the formula does not depend on it).
    pub threads: usize,
    /// Formats nested at most this deep: 1 recompresses the file alone, 2 also the data of a
    /// DEFLATE stream in it (of gzip, zlib or PDF), of an LZ4 stream or of the blocks of a
    /// LevelDB table, and so on (0 recompresses nothing).
    pub depth: u32,
    /// Decode the archive that [`compress`] makes, and store the file raw instead where it is
    /// not given back (§6). Without it, the archive's checksum of the output covers the file
    /// (R8). [`recompress`] does not verify: whoever takes its formula does.
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
    /// The most bytes of data that DEFLATE streams (of gzip, zlib and ZIP) inflate to, at every
    /// depth together, as a multiple of the file, on top of [`EXPANSION_SLACK`]: the streams
    /// past it are kept as they are.
    pub expansion: u64,
}

/// Bytes that DEFLATE streams may inflate to on top of [`Options::expansion`] times the file.
pub const EXPANSION_SLACK: u64 = 64 << 20;

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
            expansion: 32,
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

/// The contents of the modules of aqr-decompress: the data of [`FLATE`], [`LZ4`], [`SNAPPY`] and
/// [`ZIP`], scanned.
pub const CONTENTS: [Content; 4] = [
    Content { module: FLATE, input: 0, nest: Nest::Scan },
    Content { module: LZ4, input: 0, nest: Nest::Scan },
    Content { module: SNAPPY, input: 0, nest: Nest::Scan },
    Content { module: ZIP, input: 0, nest: Nest::Scan },
];

/// What recompressing a file carries along: the encoder sizing formulas, and the bytes that
/// DEFLATE streams may still inflate to ([`Options::expansion`]).
pub(crate) struct Cx {
    enc: Encoder,
    inflate: Cell<u64>,
    /// Where the formulas recompressors make are evaluated beforehand, for [`compress`].
    pre: Option<verify::Prevalidator>,
}

impl Cx {
    fn new(d: &[u8], opt: &Options) -> Cx {
        let inflate = opt.expansion.saturating_mul(d.len() as u64).saturating_add(EXPANSION_SLACK);
        Cx { enc: Encoder::with_threads(opt.level, opt.threads), inflate: Cell::new(inflate), pre: None }
    }

    /// Takes `n` bytes of inflated data from the budget, if there are as many left.
    pub(crate) fn inflate(&self, n: u64) -> bool {
        let left = self.inflate.get();
        let fits = n <= left;
        if fits {
            self.inflate.set(left - n);
        }
        fits
    }

    pub(crate) fn inflate_left(&self) -> u64 {
        self.inflate.get()
    }
}

/// `d` as a formula, with the recompressor for its format if any takes it.
pub fn recompress(d: &[u8], opt: &Options) -> Formula {
    recompress_at(d, opt, 0, opt.file, &Cx::new(d, opt))
}

/// `d` as an archive, with the checksum of `d`.
pub fn compress(d: &[u8], opt: &Options) -> Vec<u8> {
    compress_with(d, opt, false).0
}

/// [`compress`], with the formula the archive holds: that of `d` raw where it was not verified.
pub fn compress_formula(d: &[u8], opt: &Options) -> (Vec<u8>, Formula) {
    let (c, f) = compress_with(d, opt, true);
    (c, f.expect("kept"))
}

/// [`compress`], with the formula of the archive where it is to be `kept`: else it is dropped
/// before the archive is verified.
fn compress_with(d: &[u8], opt: &Options, kept: bool) -> (Vec<u8>, Option<Formula>) {
    let mut cx = Cx::new(d, opt);
    cx.pre = opt.verify.then(|| verify::Prevalidator::spawn(opt.threads, d));
    let f = recompress_at(d, opt, 0, opt.file, &cx);
    let memo = cx.pre.take().map(verify::Prevalidator::finish).unwrap_or_default();
    finish(f, d, opt, &cx, memo, kept)
}

/// The archive of formula `f` of `d`; with [`Options::verify`], decoded and compared with `d`
/// (the outputs of applications in `memo` taken from there), and where it is not `d` (a
/// module's bug), the archive of `d` raw instead. The formula is returned where `kept`; what is
/// no longer needed (the formula, the streams compressed) is dropped before the decoding.
fn finish(f: Formula, d: &[u8], opt: &Options, cx: &Cx, memo: verify::Memo, kept: bool) -> (Vec<u8>, Option<Formula>) {
    let sum = Some(aqr::checksum(d));
    let c = cx.enc.encode(&f, sum);
    cx.enc.clear();
    let f = kept.then_some(f);
    let decodes = |c: &[u8]| {
        let m = verify::Cached {
            native: Native { threads: opt.threads },
            memo: memo.into(),
            hits: Default::default(),
            file: d,
        };
        aqr::decode(c, &aqr::natives(&aqr_decompress::MODULES, &m)).is_ok_and(|out| out == d)
    };
    match c {
        Ok(c) if !opt.verify || decodes(&c) => (c, f),
        // a leaf of other bytes than `d` is not encoded with its checksum
        _ => {
            let raw = Formula::raw("raw", d.to_vec());
            (cx.enc.encode(&raw, sum).expect("a stream of the whole"), kept.then_some(raw))
        }
    }
}

/// [`recompress`] for data `depth` formats deep, within `cx`.
pub(crate) fn recompress_at(d: &[u8], opt: &Options, depth: u32, how: Nest, cx: &Cx) -> Formula {
    made(d, opt, depth, how, cx).map_or_else(|| Formula::raw("raw", d.to_vec()), |m| m.cut(d))
}

/// [`recompress_at`], unless `d` is kept raw; the data that a scan keeps between files is not
/// cut out of `d` yet.
fn made(d: &[u8], opt: &Options, depth: u32, how: Nest, cx: &Cx) -> Option<Made> {
    match how {
        _ if depth >= opt.depth => None,
        Nest::Exact => detect(d).and_then(|format| recompress_as(format, d, opt, depth, cx)).map(Made::from),
        Nest::Scan => scan(d, opt, depth, cx).filter(|m| smaller(&m.f, m.gaps.as_deref(), d, opt, cx)),
    }
}

/// A formula of data, its stream 0 perhaps yet to be cut out of the data: the bytes of `gaps`,
/// ranges of the data, one after the other. A scan's formula is so until put in place, where
/// the data between the files found is cut out of the data's own stream if it can be.
struct Made {
    f: Formula,
    gaps: Option<Vec<Range<usize>>>,
}

impl From<Formula> for Made {
    fn from(f: Formula) -> Made {
        Made { f, gaps: None }
    }
}

impl Made {
    /// The formula, cut out of `d`, the data it is of.
    fn cut(self, d: &[u8]) -> Formula {
        let Made { mut f, gaps } = self;
        if let Some(gaps) = gaps {
            let mut data = Vec::with_capacity(gaps.iter().map(Range::len).sum());
            gaps.into_iter().for_each(|g| data.extend_from_slice(&d[g]));
            f.streams[0].data = data;
        }
        f
    }

    /// [`Made::cut`] out of `d` itself, in place.
    fn cut_from(self, mut d: Vec<u8>) -> Formula {
        let Made { mut f, gaps } = self;
        if let Some(gaps) = gaps {
            let mut at = 0;
            for g in gaps {
                let n = g.len();
                if g.start != at {
                    d.copy_within(g, at);
                }
                at += n;
            }
            d.truncate(at);
            d.shrink_to_fit();
            f.streams[0].data = d;
        }
        f
    }
}

/// `d` as a file of `format`, with its contents nested, if its recompressor takes it (and the
/// formula is [`smaller`]).
fn recompress_as(format: Format, d: &[u8], opt: &Options, depth: u32, cx: &Cx) -> Option<Formula> {
    #[cfg(test)]
    tests::TRIED.with(|t| t.borrow_mut().push(format));
    let f = catch_unwind(AssertUnwindSafe(|| match format {
        Format::Pe => exe::pe(d),
        Format::MachO => exe::macho(d),
        Format::Gzip => flate::gzip(d, opt, cx),
        Format::Zlib => flate::zlib(d, opt, cx),
        Format::Lz4 => recons(aqr_lz4::formula::encode, d, opt),
        Format::Png => recons(aqr_png::formula::encode, d, opt),
        Format::Jpeg => recons(aqr_jpeg::formula::encode, d, opt),
        Format::Mp3 => recons(aqr_mp3::formula::encode, d, opt),
        Format::Flac => recons(aqr_flac::formula::encode, d, opt),
        Format::Psd => recons(aqr_psd::formula::encode, d, opt),
        Format::Pdf => recons(aqr_pdf::formula::encode, d, opt),
        Format::Sqlite => recons(aqr_sqlite::formula::encode, d, opt),
        Format::LevelDb => recons(aqr_snappy::formula::encode, d, opt),
        Format::Zip => zip::zip(d, opt, cx),
    }))
    .ok()
    .flatten()?;
    // evaluated beforehand while its contents are recompressed, which reads it only
    let shared = Arc::new(f);
    if let Some(pre) = &cx.pre {
        pre.submit(shared.clone(), d);
    }
    let children = nested(&shared, opt, depth, cx);
    let mut f = verify::reclaim(shared);
    if let Some(children) = children {
        place_all(&mut f, children, opt);
    }
    smaller(&f, None, d, opt, cx).then_some(f)
}

/// Unless [`Options::smaller`] is off, whether formula `f` of `d` archives smaller than `d`
/// raw, its stream 0 the bytes of `gaps` of `d` if given ([`Made`]).
fn smaller(f: &Formula, gaps: Option<&[Range<usize>]>, d: &[u8], opt: &Options, cx: &Cx) -> bool {
    let smaller = !opt.smaller
        || match gaps {
            Some(gaps) => {
                let pieces: Vec<&[u8]> = gaps.iter().map(|g| &d[g.clone()]).collect();
                cx.enc.smaller_with(f, 0, &pieces, d)
            }
            None => cx.enc.smaller(f, d),
        }
        .expect("slices within their streams");
    if smaller {
        // `d` is not archived as it is (`f` is, or is part of something larger kept raw)
        cx.enc.forget(d);
    }
    smaller
}

/// Bytes of the data that a scan may give to recompressors which turn it down, on top of
/// twice the data: past them, no more files are looked for.
const SCAN_SLACK: usize = 16 << 20;

/// The files in `d`, if any is found: from the first byte on, a file whose recompressor takes
/// it (up to its [`extent`]) is put in place, and the scan goes on after it; else on from the
/// next byte, so that files in one turned down are still found. Weak signatures are confirmed
/// by the extent: a zlib stream by its checksum, MPEG audio frames without a tag by one
/// following another.
fn scan(d: &[u8], opt: &Options, depth: u32, cx: &Cx) -> Option<Made> {
    scan_with(d, opt, depth, SCAN_SLACK, cx)
}

/// [`scan`] with `slack` for [`SCAN_SLACK`].
fn scan_with(d: &[u8], opt: &Options, depth: u32, slack: usize, cx: &Cx) -> Option<Made> {
    let mut found = Vec::new();
    let (mut p, mut budget) = (0, d.len().saturating_mul(2).saturating_add(slack));
    // Where each format's last file turned down ends: within it, a file of that format again
    // would be part of the same (an archive from each of its local headers, MPEG audio from
    // each of its frames), and is not tried.
    let mut turned_down: Vec<(Format, usize)> = Vec::new();
    while p < d.len() {
        // at the start, a file may also be known by its end (a LevelDB table), or be one that
        // is only ever a whole file (a SQLite database)
        let format = if p == 0 { detect(d) } else { detect::starts(&d[p..]) };
        let format = format.filter(|f| !turned_down.iter().any(|&(g, end)| g == *f && p < end));
        let child = format.filter(|_| budget > 0).and_then(|format| {
            let n = extent(format, &d[p..]).filter(|&n| n > 0)?;
            let child = recompress_as(format, &d[p..p + n], opt, depth, cx);
            if child.is_none() {
                budget = budget.saturating_sub(n);
                turned_down.retain(|&(g, _)| g != format);
                turned_down.push((format, p + n));
            }
            Some((n, child?))
        });
        match child {
            Some((n, child)) => {
                found.push((p, n, child));
                p += n;
            }
            None => p = detect::next_start(d, p + 1),
        }
    }
    // each file with the data before it, and the data after the last and the whole
    let around = description_bound(&Node::slice(0, u64::MAX, d.len() as u64));
    let sizes: Vec<_> =
        found.iter().map(|(_, n, child)| (*n, description_bound(&child.root).saturating_add(around))).collect();
    let keep = fit(around + 11, &sizes, opt.description);

    // the data between the files cut out of `d` once put in place
    let mut f = Formula::default();
    f.push("raw", Vec::new());
    let (mut gaps, mut parts, mut at, mut len) = (Vec::new(), Vec::new(), 0, 0);
    for ((p, n, child), keep) in found.into_iter().zip(keep) {
        if keep {
            parts.push(Node::slice(0, len as u64, (p - at) as u64));
            gaps.push(at..p);
            len += p - at;
            parts.push(f.embed(child));
            at = p + n;
        }
    }
    if parts.is_empty() {
        return None;
    }
    parts.push(Node::slice(0, len as u64, (d.len() - at) as u64));
    gaps.push(at..d.len());
    f.root = Node::concat(parts);
    Some(Made { f, gaps: Some(gaps) })
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

/// The formulas of the contents of the applications in `f` ([`Options::contents`]), a formula
/// of data `depth` formats deep, to put in place of their slices ([`place_all`]) where they have
/// any application (as many as fit in [`Options::description`]); `None` where none has.
fn nested(f: &Formula, opt: &Options, depth: u32, cx: &Cx) -> Option<Vec<Option<Made>>> {
    if depth + 1 >= opt.depth {
        return None;
    }
    let mut children = Vec::new();
    contents(&f.root, f, opt, depth, cx, &mut children);
    let sizes: Vec<_> = children
        .iter()
        .map(|(slice, child)| match child {
            Some(c) => (slice.len() as usize, description_bound(&c.f.root).saturating_sub(description_bound(slice))),
            None => (0, 0),
        })
        .collect();
    let keep = fit(description_bound(&f.root), &sizes, opt.description);
    if !children.iter().zip(&keep).any(|((_, child), &keep)| keep && child.is_some()) {
        return None;
    }
    Some(children.into_iter().zip(keep).map(|((_, child), keep)| child.filter(|_| keep)).collect())
}

/// Puts the formulas of [`nested`] in place in `f`; the bytes left without a slice are dropped.
fn place_all(f: &mut Formula, children: Vec<Option<Made>>, opt: &Options) {
    let mut slices = vec![0; f.streams.len()];
    f.root.visit(&mut |n| {
        if let &Node::Slice { stream, .. } = n
            && let Some(n) = slices.get_mut(stream)
        {
            *n += 1;
        }
    });
    let mut root = std::mem::replace(&mut f.root, Node::Concat(Vec::new()));
    place(&mut root, f, &slices, opt, &mut children.into_iter());
    f.root = root;
    f.compact();
}

/// The contents below node `n` of `f`, in order: each slice, and its formula where it has any
/// application.
fn contents(n: &Node, f: &Formula, opt: &Options, depth: u32, cx: &Cx, out: &mut Vec<(Node, Option<Made>)>) {
    match n {
        Node::Slice { .. } => {}
        Node::Concat(ns) => ns.iter().for_each(|n| contents(n, f, opt, depth, cx, out)),
        Node::Apply(a) => {
            for (i, input) in a.inputs.iter().enumerate() {
                match (input, content(opt, a.module, i)) {
                    (&Node::Slice { stream, start, len }, Some(content)) => {
                        let d = f.streams.get(stream).and_then(|s| s.data.get(start as usize..)?.get(..len as usize));
                        let child = d.and_then(|d| made(d, opt, depth + 1, content.nest, cx));
                        out.push((input.clone(), child.filter(|c| !c.f.applies().is_empty())));
                    }
                    _ => contents(input, f, opt, depth, cx, out),
                }
            }
        }
    }
}

/// Puts the formulas of the contents below node `n` of `f`, in the order of [`contents`], in
/// place of their slices: cut out of the stream of their data itself where they are all of it,
/// the only slice of it (of the `slices` of each stream).
fn place(
    n: &mut Node,
    f: &mut Formula,
    slices: &[usize],
    opt: &Options,
    children: &mut impl Iterator<Item = Option<Made>>,
) {
    match n {
        Node::Slice { .. } => {}
        Node::Concat(ns) => ns.iter_mut().for_each(|n| place(n, f, slices, opt, children)),
        Node::Apply(a) => {
            let module = a.module;
            for (i, input) in a.inputs.iter_mut().enumerate() {
                match (&*input, content(opt, module, i)) {
                    (&Node::Slice { stream, start, len }, Some(_)) => {
                        if let Some(child) = children.next().flatten() {
                            // (a child is of a slice within its stream)
                            let data = &mut f.streams[stream].data;
                            let child = if slices[stream] == 1 && start == 0 && len == data.len() as u64 {
                                child.cut_from(std::mem::take(data))
                            } else {
                                child.cut(&data[start as usize..][..len as usize])
                            };
                            *input = f.embed(child);
                        }
                    }
                    _ => place(input, f, slices, opt, children),
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
/// FORMAT.md), if it makes one. Whether coding pays off is the recompressor's call: the
/// streams are not compressed yet, so their size says little.
fn recons(encode: fn(&[u8], usize) -> Option<Formula>, d: &[u8], opt: &Options) -> Option<Formula> {
    encode(d, opt.threads)
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

    thread_local! {
        /// The formats given to recompressors, in order.
        pub(crate) static TRIED: std::cell::RefCell<Vec<Format>> = const { std::cell::RefCell::new(Vec::new()) };
    }

    /// The formulas recompressors make are evaluated beforehand: decoding the archive takes
    /// every application's output from the memo, and runs the module where it is not there.
    #[test]
    fn verified_beforehand() {
        let text: Vec<u8> = (0..20000).flat_map(|i| format!("line {i} of the text\n").into_bytes()).collect();
        let mut z = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::new(6));
        std::io::Write::write_all(&mut z, &text).unwrap();
        let d = z.finish().unwrap();
        let opt = Options { smaller: false, ..Options::default() };
        let mut cx = Cx::new(&d, &opt);
        cx.pre = Some(verify::Prevalidator::spawn(2, &d));
        let f = recompress_at(&d, &opt, 0, opt.file, &cx);
        let memo = cx.pre.take().unwrap().finish();
        assert!(!f.applies().is_empty());
        // the DEFLATE stream is not kept again, but known to be the file's
        assert_eq!(memo.bytes(), 0);
        let c = cx.enc.encode(&f, Some(aqr::checksum(&d))).unwrap();
        let decode = |c: &[u8], memo: verify::Memo| {
            let m =
                verify::Cached { native: Native { threads: 1 }, memo: memo.into(), hits: Default::default(), file: &d };
            let out = aqr::decode(c, &aqr::natives(&aqr_decompress::MODULES, &m));
            (out, m.hits.into_inner().unwrap())
        };
        let other = compress(&d[..d.len() - 1], &Options { verify: false, ..opt.clone() });
        // another archive's applications are not in it
        let (memo, hits) = {
            let m =
                verify::Cached { native: Native { threads: 1 }, memo: memo.into(), hits: Default::default(), file: &d };
            aqr::decode(&other, &aqr::natives(&aqr_decompress::MODULES, &m)).unwrap();
            (m.memo.into_inner().unwrap(), m.hits.into_inner().unwrap())
        };
        assert_eq!(hits, 0);
        assert_eq!(decode(&c, memo), (Ok(d.clone()), f.applies().len()));
        // without the memo, the same, with the modules run
        assert_eq!(decode(&c, Default::default()), (Ok(d.clone()), 0));
    }

    /// The data between the files a scan found is cut out of the stream of the data in place
    /// where the stream is all and only the content, else copied out of it.
    #[test]
    fn gaps_cut_in_place() {
        // large enough to be cut to its length where it is (as allocators do)
        let n = 1 << 20;
        let data: Vec<u8> = (0..n).map(|i| (i % 251) as u8).collect();
        let gaps = [&data[2..5], &data[16..]].concat();
        let child = || {
            let mut f = Formula::default();
            f.push("raw", Vec::new());
            let found = f.push("found", b"FOUND".to_vec());
            f.root = Node::concat([Node::slice(0, 0, 3), found, Node::slice(0, 3, n as u64 - 16)]);
            Made { f, gaps: Some(vec![2..5, 16..n]) }
        };
        let mut f = Formula::default();
        let whole = f.push("raw", data.clone());
        let record = f.push("flate/record", vec![0]);
        f.root = Node::apply(FLATE, vec![], vec![whole.clone(), record], n as u64);
        // the stream sliced again
        let mut copied = f.clone();
        copied.root = Node::concat([f.root.clone(), whole]);

        let at = f.streams[0].data.as_ptr();
        place_all(&mut f, vec![Some(child())], &Options::default());
        let cut = f.streams.iter().find(|s| s.class == "raw").unwrap();
        assert!(cut.data.as_ptr() == at && cut.data == gaps);
        assert_eq!(f.streams.len(), 3);

        place_all(&mut copied, vec![Some(child())], &Options::default());
        let raw: Vec<_> = copied.streams.iter().filter(|s| s.class == "raw").map(|s| &s.data[..]).collect();
        assert!(raw == [&data[..], &gaps[..]]);
    }

    /// An archive that does not give the file back is replaced by that of the file raw.
    #[test]
    fn finish_falls_back() {
        let d = b"some data, kept raw where its formula is wrong".repeat(100);
        let opt = Options::default();
        let cx = Cx::new(&d, &opt);
        let mut wrong = Formula::raw("raw", d.clone());
        wrong.streams[0].data[7] ^= 1;
        let raw = aqr::encode(&Formula::raw("raw", d.clone()), Some(aqr::checksum(&d)), opt.level).unwrap();
        // a leaf, whose checksum is not the output's, is not encoded at all
        assert_eq!(
            finish(wrong.clone(), &d, &Options { verify: false, ..opt.clone() }, &cx, Default::default(), false).0,
            raw
        );
        // two slices: the archive has the output's checksum
        wrong.root = Node::Concat(vec![Node::slice(0, 0, 100), Node::slice(0, 100, d.len() as u64 - 100)]);
        assert_eq!(finish(wrong.clone(), &d, &opt, &cx, Default::default(), false).0, raw);
        // a module that fails
        let fails = Formula {
            streams: vec![aqr_formula::Stream { class: "raw".into(), data: vec![1, 2, 3] }],
            root: Node::apply(FLATE, vec![], vec![Node::slice(0, 0, 3), Node::slice(0, 0, 3)], d.len() as u64),
        };
        assert_eq!(finish(fails, &d, &opt, &cx, Default::default(), false).0, raw);
        // without verification, as it is
        let opt = Options { verify: false, ..opt };
        assert_eq!(
            aqr_decompress::decompress(&finish(wrong, &d, &opt, &cx, Default::default(), false).0, 1),
            Err(aqr::Failure::Invalid("output checksum mismatch".into()))
        );
    }

    /// An archive turned down is not tried again from each of its later local headers, where an
    /// archive also starts and runs to the same end record.
    #[test]
    fn scan_after_turned_down() {
        let (mut d, mut cd) = (Vec::new(), Vec::new());
        for i in 0..8u32 {
            let data = aqr_common::rng::noise(5000 + i as usize);
            // encrypted: kept as they are, so that the archive is turned down
            let mut h = vec![20, 0, 1, 0, 0, 0, 0, 0, 0, 0];
            h.extend_from_slice(&crc32fast::hash(&data).to_le_bytes());
            h.extend_from_slice(&(data.len() as u32).to_le_bytes());
            h.extend_from_slice(&(data.len() as u32).to_le_bytes());
            h.extend_from_slice(&[1, 0, 0, 0]);
            cd.extend_from_slice(b"PK\x01\x02\x14\x03");
            cd.extend_from_slice(&h);
            cd.extend_from_slice(&[0; 10]);
            cd.extend_from_slice(&(d.len() as u32).to_le_bytes());
            cd.push(b'a' + i as u8);
            d.extend_from_slice(b"PK\x03\x04");
            d.extend_from_slice(&h);
            d.push(b'a' + i as u8);
            d.extend_from_slice(&data);
        }
        let at = d.len() as u32;
        d.extend_from_slice(&cd);
        d.extend_from_slice(b"PK\x05\x06\0\0\0\0\x08\0\x08\0");
        d.extend_from_slice(&(cd.len() as u32).to_le_bytes());
        d.extend_from_slice(&at.to_le_bytes());
        d.extend_from_slice(&[0, 0]);
        TRIED.with(|t| t.borrow_mut().clear());
        let f = recompress(&d, &Options::default());
        assert!(f.applies().is_empty());
        let zips = TRIED.with(|t| t.borrow().iter().filter(|&&f| f == Format::Zip).count());
        assert_eq!(zips, 1);
    }

    /// Data given to recompressors that turn it down counts against a scan, which stops
    /// looking for files past twice the data.
    #[test]
    fn scan_budget() {
        let jpeg = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../aqr-jpeg/tests/data/gradient.jpg")).unwrap();
        // files of different formats running to the end of the data, each turned down (one of
        // a format already turned down there would not be tried)
        let fakes: [&[u8]; 3] = [
            &[&b"\x89PNG\r\n\x1a\n"[..], &0x7fff_0000u32.to_be_bytes(), b"IHDR"].concat(),
            b"fLaC\x80\0\0\x22",
            b"%PDF-1.7\n",
        ];
        let data = |n: usize| [fakes[..n].concat(), vec![0; 10000], jpeg.clone()].concat();
        let found = |d: &[u8]| {
            let opt = Options::default();
            scan_with(d, &opt, 0, 0, &Cx::new(d, &opt)).map_or(0, |m| m.f.applies().len())
        };
        assert_eq!(found(&data(1)), 1);
        assert_eq!(found(&data(3)), 0);
    }
}
