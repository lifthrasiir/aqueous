//! Recompression of a file into an entry formula (xform-design.md §4.2, §6): [`detect`] picks
//! the recompressor by the magic bytes of a file, and [`recompress`] runs it on each file found,
//! giving a [`Formula`] rather than bytes.
//!
//! - PE images, and Mach-O images (thin or universal): aqr-exe's x86 disfilter or ARM64
//!   filter applied to each executable section, over its streams; the rest of the file is
//!   sliced in between, a content stream (below).
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
//! of the data. So are the slices of a stream that a recompressor gives as data as it is
//! ([`Stream::content`]: the rest of an executable, around its code), wherever they are.
//!
//! Data is taken as one file by the format it starts like ([`Nest::Exact`]), or scanned for
//! files starting anywhere in it ([`Nest::Scan`], the default): each is cut to its end
//! ([`extent`]), and the data between them kept raw, in one stream. Data that recompressors
//! turn down counts against a scan, which gives up past twice the data (and 16 MiB).
//!
//! What a recompressor knows of the data it gives, the [`Kind`] of its stream, refines that
//! ([`Nest::of`]): pixels, text, fonts, tables and compressed data are not looked into, and
//! data of [`Kind::Format`] is taken as a file of that format from its start ([`Nest::As`]).
//! The contents of an application may be slices of several streams one after the other (a
//! ZIP archive's entries, by kind), each recompressed by its own kind. aqr-zip gives the
//! entries their kinds by their names ([`kind_by_name`]), as gzip's data is by its name, and
//! aqr-pdf the streams of a PDF file by what they are.
//!
//! Contents that are PNG files of the same geometry and alike (variants of a picture, with
//! layers shown or hidden) are coded against each other, as one set of an output per file
//! (`sets`), where that is smaller than coding them alone.
//!
//! A file that a scan finds but its recompressor turns down is looked into by its parts where
//! the recompressor tells them ([`Region`]; aqr-psd does): the parts of a format are taken as
//! files of it, the parts of the other kinds are not looked into, and only the bytes no part
//! tells of (and parts of unknown kind) are scanned. Else the scan goes on from its next byte.
//!
//! What DEFLATE streams inflate to, at every depth together, is kept within
//! [`Options::expansion`] times the file (and 64 MiB): past it, streams are kept as they are,
//! so that nested archives (42.zip) and bombs cost no more than that.
//!
//! Before the formula is archived, its streams of each class and kind are put one after
//! another in one stream ([`Formula::merge_classes`]), as they are alike: the same
//! recompressor's, of the files of a container.
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
//! [`Native`] evaluates the formulas.
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
mod sets;
mod verify;
mod zip;

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ops::Range;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

pub use aqr_formula::{Formula, Kind, Module, Node, Region};
pub use detect::{Format, detect, kind_by_name};
pub use extent::extent;

use aqr_decompress::{FLATE, LZ4, Native, SNAPPY, ZIP};
use aqr_formula::Stream;
use aqr_formula::aqr::{self, Encoder, Level, description_bound};
use aqr_formula::probe::{Look, Probe};

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
    /// Size formulas without their resources ([`aqr::Encoder::share_resources`]), as for an
    /// archive of many files sharing them: for measuring what they come to in the limit.
    pub shared_resources: bool,
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
    /// As data starting with a file of the format, which is not looked for: where its
    /// recompressor takes it, as [`Nest::Scan`] would, the data after it scanned; else looked
    /// into by its parts where the recompressor tells them ([`Region`]), or kept raw.
    As(Format),
}

impl Nest {
    /// How data of `kind` is recompressed where it is otherwise as `self`, if at all. No file
    /// is in pixels, text, a font or a table, and none that a recompressor would take in
    /// compressed data: what a scan would find there is there by chance (MPEG audio headers in
    /// pixels, zlib headers in text), and costs the scan its time.
    pub fn of(self, kind: Kind) -> Option<Nest> {
        match kind {
            Kind::Unknown => Some(self),
            Kind::Pixels | Kind::Text | Kind::Font | Kind::Table | Kind::Compressed => None,
            Kind::Format(name) => Some(Format::from_name(name).map_or(self, Nest::As)),
        }
    }
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
            shared_resources: false,
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
    /// Formulas made beforehand (of [`pure`] formats), by the data (where it is, its length)
    /// and the format.
    ready: RefCell<HashMap<(usize, usize, Format), Option<Formula>>>,
}

impl Cx {
    fn new(d: &[u8], opt: &Options) -> Cx {
        let inflate = opt.expansion.saturating_mul(d.len() as u64).saturating_add(EXPANSION_SLACK);
        let mut enc = Encoder::with_threads(opt.level, opt.threads);
        if opt.shared_resources {
            enc.share_resources();
        }
        Cx { enc, inflate: Cell::new(inflate), pre: None, ready: Default::default() }
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
fn finish(
    mut f: Formula,
    d: &[u8],
    opt: &Options,
    cx: &Cx,
    memo: verify::Memo,
    kept: bool,
) -> (Vec<u8>, Option<Formula>) {
    // the streams of a class together, as they compress smaller
    f.merge_classes();
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
        Nest::Scan => scan(d, None, opt, depth, cx).filter(|m| smaller(&m.f, m.gaps.as_deref(), d, opt, cx)),
        Nest::As(format) => {
            scan(d, Some(format), opt, depth, cx).filter(|m| smaller(&m.f, m.gaps.as_deref(), d, opt, cx))
        }
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
    recompress_with(format, d, Kind::Unknown, opt, depth, cx)
}

/// [`recompress_as`], the data of a zlib stream or gzip member (without a name) of kind
/// `contents`.
fn recompress_with(format: Format, d: &[u8], contents: Kind, opt: &Options, depth: u32, cx: &Cx) -> Option<Formula> {
    #[cfg(test)]
    tests::TRIED.with(|t| t.borrow_mut().push(format));
    let ready = cx.ready.borrow_mut().remove(&(d.as_ptr() as usize, d.len(), format));
    let f = match ready {
        Some(f) => f,
        None => pure(format, d, opt.threads).unwrap_or_else(|| {
            catch_unwind(AssertUnwindSafe(|| match format {
                Format::Pe => exe::pe(d),
                Format::MachO => exe::macho(d),
                Format::Gzip => flate::gzip(d, contents, opt, depth, cx),
                Format::Zlib => flate::zlib(d, contents, opt, depth, cx),
                Format::Zip => zip::zip(d, opt, depth, cx),
                _ => unreachable!("a pure format"),
            }))
            .ok()
            .flatten()
        }),
    }?;
    // evaluated beforehand while its contents are recompressed, which reads it only
    let shared = Arc::new(f);
    if let Some(pre) = &cx.pre {
        pre.submit(shared.clone(), d);
    }
    let children = nested(&shared, opt, depth, cx);
    let mut f = verify::reclaim(shared);
    if let Some(mut children) = children {
        let sets = sets::sets(&f, &mut children, opt);
        place_all(&mut f, children.into_iter().map(|c| c.1).collect(), sets, opt);
    }
    smaller(&f, None, d, opt, cx).then_some(f)
}

/// Unless [`Options::smaller`] is off, what tells whether recompressing a stream may pay,
/// for its data `depth` formats deep: at the level of the archive, files looked for as
/// [`Nest::of`] tells (scanned, unless nested formats would go deeper than allowed).
pub(crate) fn probe(opt: &Options, depth: u32) -> Option<Probe> {
    let look = |kind| match Nest::Scan.of(kind) {
        None => Look::No,
        Some(Nest::As(_)) => Look::Whole,
        Some(_) => Look::Scan,
    };
    let starts = |d: &[u8]| detect(d).is_some();
    let probe = Probe { level: opt.level, look, starts, scan: files_len };
    opt.smaller.then_some(if depth + 1 < opt.depth { probe } else { Probe::flat(opt.level) })
}

/// Bytes of the files of formats that recompressors take in `d`, as a scan would find them:
/// by their signatures, then their ends ([`extent`]), each from the end of the one before.
fn files_len(d: &[u8]) -> usize {
    let len = |p: usize, f: Option<Format>| f.and_then(|f| extent(f, &d[p..])).unwrap_or(0);
    let mut total = len(0, detect(d));
    let mut p = detect::next_start(d, total.max(1));
    while p < d.len() {
        let n = len(p, detect::starts(&d[p..]));
        total += n;
        p = detect::next_start(d, p + n.max(1));
    }
    total
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
/// it (up to its [`extent`]) is put in place, and the scan goes on after it; else, so that
/// files in one turned down are still found, within it by its parts where its recompressor
/// tells them ([`within`]), or on from the next byte. Weak signatures are confirmed by the
/// extent: a zlib stream by its checksum, MPEG audio frames without a tag by one following
/// another. Where the data is known to start with a file of `first` ([`Nest::As`]), it is that
/// file, or none where that is turned down and tells nothing of its parts.
fn scan(d: &[u8], first: Option<Format>, opt: &Options, depth: u32, cx: &Cx) -> Option<Made> {
    scan_with(d, first, opt, depth, SCAN_SLACK, cx)
}

/// [`scan`] with `slack` for [`SCAN_SLACK`].
fn scan_with(d: &[u8], first: Option<Format>, opt: &Options, depth: u32, slack: usize, cx: &Cx) -> Option<Made> {
    let found = find(d, first, opt, depth, slack, cx)?;
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

/// A file found in data: where it starts, its length and its formula.
type Found = (usize, usize, Formula);

/// The files of [`scan_with`], in order; `None` where `d` starts with a file of `first` that is
/// turned down and whose parts are not told ([`regions`]). Within a file turned down whose parts
/// are told, files are looked for by them ([`within`]), else from each byte on.
fn find(d: &[u8], first: Option<Format>, opt: &Options, depth: u32, slack: usize, cx: &Cx) -> Option<Vec<Found>> {
    let mut found = Vec::new();
    let (mut p, mut budget) = (0, d.len().saturating_mul(2).saturating_add(slack));
    // Where each format's last file turned down ends: within it, a file of that format again
    // would be part of the same (an archive from each of its local headers, MPEG audio from
    // each of its frames), and is not tried.
    let mut turned_down: Vec<(Format, usize)> = Vec::new();
    while p < d.len() {
        // at the start, a file may also be known by its end (a LevelDB table), or be one that
        // is only ever a whole file (a SQLite database)
        let format = match first {
            Some(f) if p == 0 => Some(f),
            _ if p == 0 => detect(d),
            _ => detect::starts(&d[p..]),
        };
        let format = format.filter(|f| !turned_down.iter().any(|&(g, end)| g == *f && p < end));
        let mut down = None;
        let child = format.filter(|_| budget > 0).and_then(|format| {
            let n = extent(format, &d[p..]).filter(|&n| n > 0)?;
            let child = recompress_as(format, &d[p..p + n], opt, depth, cx);
            if child.is_none() {
                budget = budget.saturating_sub(n);
                turned_down.retain(|&(g, _)| g != format);
                turned_down.push((format, p + n));
                down = Some((format, n));
            }
            Some((n, child?))
        });
        match child {
            Some((n, child)) => {
                found.push((p, n, child));
                p += n;
            }
            None => match down.and_then(|(format, n)| Some((n, regions(format, &d[p..p + n])?))) {
                Some((n, parts)) => {
                    let inner = within(&d[p..p + n], &parts, opt, depth, cx);
                    found.extend(inner.into_iter().map(|(q, m, f)| (p + q, m, f)));
                    p += n;
                }
                None if p == 0 && first.is_some() => return None,
                None => p = detect::next_start(d, p + 1),
            },
        }
    }
    Some(found)
}

/// The files in `d`, a file turned down whose parts are `parts` ([`regions`]), in order: a part
/// of a format is that file, taken as one up to its [`extent`] (by its parts again where turned
/// down; what follows its end in the part is its own, and not looked into), the bytes no part
/// tells of and parts of unknown kind are scanned, and the other parts are not looked into
/// ([`Nest::of`]).
fn within(d: &[u8], parts: &[Region], opt: &Options, depth: u32, cx: &Cx) -> Vec<Found> {
    let mut found = Vec::new();
    // (not from the first byte, where `d` itself starts)
    let scan_part = |found: &mut Vec<Found>, r: Range<usize>| {
        let r = r.start.max(1)..r.end;
        if !r.is_empty()
            && let Some(inner) = find(&d[r.clone()], None, opt, depth, SCAN_SLACK, cx)
        {
            found.extend(inner.into_iter().map(|(q, n, f)| (r.start + q, n, f)));
        }
    };
    let mut at = 0;
    for part in parts {
        let r = part.span.clone();
        if r.start < at || r.end > d.len() {
            continue;
        }
        scan_part(&mut found, at..r.start);
        match Nest::Scan.of(part.kind) {
            None => {}
            Some(Nest::As(format)) => {
                let file = &d[r.clone()];
                let n = extent(format, file).filter(|&n| n > 0).unwrap_or(file.len());
                match recompress_with(format, &file[..n], part.contents, opt, depth, cx) {
                    Some(f) => found.push((r.start, n, f)),
                    None => {
                        let inner = regions(format, &file[..n]).map(|p| within(&file[..n], &p, opt, depth, cx));
                        found.extend(inner.into_iter().flatten().map(|(q, m, f)| (r.start + q, m, f)));
                    }
                }
            }
            Some(_) => scan_part(&mut found, r.clone()),
        }
        at = r.end;
    }
    scan_part(&mut found, at..d.len());
    found
}

/// What the parts of `d`, a file of `format`, are, where its recompressor tells.
fn regions(format: Format, d: &[u8]) -> Option<Vec<Region>> {
    catch_unwind(AssertUnwindSafe(|| match format {
        Format::Psd => aqr_psd::formula::regions(d),
        _ => None,
    }))
    .ok()
    .flatten()
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

/// The contents of the applications in `f` ([`Options::contents`]), a formula of data `depth`
/// formats deep, each its slice and its formula to put in place of the slice ([`place_all`])
/// where it has any application (as many as fit in [`Options::description`]); `None` where
/// none has.
fn nested(f: &Formula, opt: &Options, depth: u32, cx: &Cx) -> Option<Vec<(Node, Option<Made>)>> {
    if depth + 1 >= opt.depth {
        return None;
    }
    let prepared = prepare(f, opt, cx);
    let mut children = Vec::new();
    contents(&f.root, f, opt, depth, cx, &mut children);
    // (those not taken: the contents' own nested contents make and take their own)
    let mut ready = cx.ready.borrow_mut();
    prepared.iter().for_each(|k| drop(ready.remove(k)));
    drop(ready);
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
    Some(children.into_iter().zip(keep).map(|((slice, child), keep)| (slice, child.filter(|_| keep))).collect())
}

/// What takes the place of a content's slice: its formula, or an output of a set.
enum Placed {
    Made(Made),
    Node(Node),
}

/// Puts the formulas of [`nested`] in place in `f`, and the outputs of the `sets` of them
/// ([`sets::sets`], whose formulas are `None` in `children`); the bytes left without a slice
/// are dropped.
fn place_all(f: &mut Formula, children: Vec<Option<Made>>, sets: Vec<sets::Set>, opt: &Options) {
    let mut outputs: Vec<Option<Node>> = (0..children.len()).map(|_| None).collect();
    for s in sets {
        let Node::Concat(outs) = f.embed(s.f) else { unreachable!("a set's root is its outputs") };
        s.members.into_iter().zip(outs).for_each(|(m, o)| outputs[m] = Some(o));
    }
    let children = children.into_iter().zip(outputs).map(|(c, o)| match o {
        Some(o) => Some(Placed::Node(o)),
        None => c.map(Placed::Made),
    });
    // (of the applications of several outputs too, which are not looked into)
    let mut slices = vec![0; f.streams.len()];
    f.visit(&mut |n| {
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
/// application. A content is an input of an application ([`Options::contents`]) or a slice of a
/// stream that is [`Stream::content`], scanned.
fn contents(n: &Node, f: &Formula, opt: &Options, depth: u32, cx: &Cx, out: &mut Vec<(Node, Option<Made>)>) {
    match n {
        &Node::Slice { stream, .. } if is_content(f, stream) => content_slices(n, Nest::Scan, f, opt, depth, cx, out),
        // (patches are made of recompressed data, not looked into)
        Node::Slice { .. } | Node::Output { .. } | Node::Patch(_) => {}
        Node::Concat(ns) | Node::Pack(ns) => ns.iter().for_each(|n| contents(n, f, opt, depth, cx, out)),
        Node::Apply(a) => {
            for (i, input) in a.inputs.iter().enumerate() {
                match content(opt, a.module, i) {
                    Some(content) => content_slices(input, content.nest, f, opt, depth, cx, out),
                    None => contents(input, f, opt, depth, cx, out),
                }
            }
        }
    }
}

/// [`contents`] of node `n`, a content recompressed as `nest`: each of its slices is a content
/// of the kind of its stream, and the applications in it have contents of their own.
fn content_slices(
    n: &Node,
    nest: Nest,
    f: &Formula,
    opt: &Options,
    depth: u32,
    cx: &Cx,
    out: &mut Vec<(Node, Option<Made>)>,
) {
    match n {
        &Node::Slice { stream, start, len } => {
            let child = f.streams.get(stream).and_then(|s| {
                let d = s.data.get(start as usize..)?.get(..len as usize)?;
                made(d, opt, depth + 1, nest.of(s.kind)?, cx)
            });
            out.push((n.clone(), child.filter(|c| !c.f.applies().is_empty())));
        }
        Node::Concat(ns) | Node::Pack(ns) => ns.iter().for_each(|n| content_slices(n, nest, f, opt, depth, cx, out)),
        Node::Apply(_) => contents(n, f, opt, depth, cx, out),
        Node::Output { .. } | Node::Patch(_) => {}
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
    children: &mut impl Iterator<Item = Option<Placed>>,
) {
    match n {
        &mut Node::Slice { stream, .. } if is_content(f, stream) => place_slices(n, f, slices, opt, children),
        Node::Slice { .. } | Node::Output { .. } | Node::Patch(_) => {}
        Node::Concat(ns) | Node::Pack(ns) => ns.iter_mut().for_each(|n| place(n, f, slices, opt, children)),
        Node::Apply(a) => {
            let module = a.module;
            for (i, input) in a.inputs.iter_mut().enumerate() {
                match content(opt, module, i) {
                    Some(_) => place_slices(input, f, slices, opt, children),
                    None => place(input, f, slices, opt, children),
                }
            }
        }
    }
}

/// [`place`] in node `n`, a content: in the order of [`content_slices`].
fn place_slices(
    n: &mut Node,
    f: &mut Formula,
    slices: &[usize],
    opt: &Options,
    children: &mut impl Iterator<Item = Option<Placed>>,
) {
    match n {
        &mut Node::Slice { stream, start, len } => match children.next().flatten() {
            Some(Placed::Node(o)) => *n = o,
            Some(Placed::Made(child)) => {
                // (a child is of a slice within its stream)
                let data = &mut f.streams[stream].data;
                let child = if slices[stream] == 1 && start == 0 && len == data.len() as u64 {
                    child.cut_from(std::mem::take(data))
                } else {
                    child.cut(&data[start as usize..][..len as usize])
                };
                *n = f.embed(child);
            }
            None => {}
        },
        Node::Concat(ns) | Node::Pack(ns) => ns.iter_mut().for_each(|n| place_slices(n, f, slices, opt, children)),
        Node::Apply(_) => place(n, f, slices, opt, children),
        Node::Output { .. } | Node::Patch(_) => {}
    }
}

/// Whether `stream` of `f` is [`Stream::content`].
fn is_content(f: &Formula, stream: usize) -> bool {
    f.streams.get(stream).is_some_and(|s| s.content)
}

/// The content that input `i` of `m` is, if any.
fn content(opt: &Options, m: Module, i: usize) -> Option<&Content> {
    opt.contents.iter().find(|c| c.module == m && c.input == i)
}

/// The formula that the recompressor of `format` makes of `d` (its `formula` module, see its
/// FORMAT.md), if it makes one, where it is a function of `d` alone (it looks for no files in
/// its data, and takes nothing of a budget): `None` for the other formats. Whether coding pays
/// off is the recompressor's call: the streams are not compressed yet, so their size says
/// little. The formula is the same whatever `threads` is.
fn pure(format: Format, d: &[u8], threads: usize) -> Option<Option<Formula>> {
    let encode = pure_encode(format)?;
    #[cfg(test)]
    tests::PURE.with(|n| n.set(n.get() + 1));
    Some(catch_unwind(AssertUnwindSafe(|| encode(d, threads))).ok().flatten())
}

/// Whether `format` is one of [`pure`].
fn is_pure(format: Format) -> bool {
    pure_encode(format).is_some()
}

/// A recompressor's `encode`: the formula of the data, on so many threads.
type Encode = fn(&[u8], usize) -> Option<Formula>;

/// The `encode` of the recompressor of a [`pure`] format.
fn pure_encode(format: Format) -> Option<Encode> {
    Some(match format {
        Format::Lz4 => aqr_lz4::formula::encode,
        Format::Png => aqr_png::formula::encode,
        Format::Jpeg => aqr_jpeg::formula::encode,
        Format::Mp3 => aqr_mp3::formula::encode,
        Format::Flac => aqr_flac::formula::encode,
        Format::Raw => aqr_raw::formula::encode,
        Format::Psd => aqr_psd::formula::encode,
        Format::Pdf => aqr_pdf::formula::encode,
        Format::Sqlite => aqr_sqlite::formula::encode,
        Format::LevelDb => aqr_snappy::formula::encode,
        _ => return None,
    })
}

/// Makes the formulas of the files of [`pure`] formats among the contents of `f` beforehand,
/// at once (as [`contents`] would take them in turn), into [`Cx::ready`]; returns their keys.
fn prepare(f: &Formula, opt: &Options, cx: &Cx) -> Vec<(usize, usize, Format)> {
    let mut jobs = Vec::new();
    pure_contents(&f.root, None, f, opt, &mut jobs);
    if jobs.len() < 2 {
        return Vec::new();
    }
    let workers = opt.threads.min(jobs.len()).max(1);
    let inner = (opt.threads / workers).max(1);
    let made = sets::par_map(jobs.len(), workers, |i| pure(jobs[i].0, jobs[i].1, inner).flatten());
    let mut ready = cx.ready.borrow_mut();
    let keys: Vec<_> = jobs.iter().map(|&(format, d)| (d.as_ptr() as usize, d.len(), format)).collect();
    for (&k, f) in keys.iter().zip(made) {
        ready.insert(k, f);
    }
    keys
}

/// The files of [`pure`] formats below node `n` of `f` that [`contents`] (with `nest` `None`)
/// or [`content_slices`] (with it) would take as such, each with its data as it is given its
/// recompressor (to its [`extent`]).
fn pure_contents<'a>(n: &Node, nest: Option<Nest>, f: &'a Formula, opt: &Options, out: &mut Vec<(Format, &'a [u8])>) {
    match (n, nest) {
        (&Node::Slice { stream, .. }, None) if is_content(f, stream) => pure_contents(n, Some(Nest::Scan), f, opt, out),
        (&Node::Slice { stream, start, len }, Some(nest)) => {
            let Some(s) = f.streams.get(stream) else { return };
            let Some(d) = s.data.get(start as usize..).and_then(|d| d.get(..len as usize)) else { return };
            if let Some(Nest::As(format)) = nest.of(s.kind)
                && is_pure(format)
                && let Some(n) = extent(format, d).filter(|&n| n > 0)
            {
                out.push((format, &d[..n]));
            }
        }
        (Node::Concat(ns) | Node::Pack(ns), _) => ns.iter().for_each(|n| pure_contents(n, nest, f, opt, out)),
        (Node::Apply(a), _) => {
            for (i, input) in a.inputs.iter().enumerate() {
                pure_contents(input, content(opt, a.module, i).map(|c| c.nest), f, opt, out);
            }
        }
        _ => {}
    }
}

/// Bytes of the streams of `f` by class, largest first.
pub fn classes(f: &Formula) -> Vec<(&str, usize)> {
    let mut out: Vec<(&str, usize)> = Vec::new();
    for Stream { class, data, .. } in &f.streams {
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
        /// The DEFLATE streams of gzip and zlib deconstructed.
        pub(crate) static DECONSTRUCTED: Cell<usize> = const { Cell::new(0) };
        /// The files of [`pure`] formats given to their recompressors.
        pub(crate) static PURE: Cell<usize> = const { Cell::new(0) };
    }

    /// The contents of pure formats are made beforehand, each once: those of a ZIP archive's
    /// entries are not made again where the first entry's own contents are looked for.
    #[test]
    fn contents_made_once() {
        let fixture = |p: &str| std::fs::read(format!("{}/../{p}", env!("CARGO_MANIFEST_DIR"))).unwrap();
        let files =
            [fixture("aqr-jpeg/tests/data/gradient.jpg"), fixture("aqr-jpeg/tests/data/prog_script_recode.jpg")];
        let (mut d, mut cd) = (Vec::new(), Vec::new());
        for (i, data) in files.iter().enumerate() {
            let mut h = vec![20, 0, 0, 0, 0, 0, 0, 0, 0, 0];
            h.extend_from_slice(&crc32fast::hash(data).to_le_bytes());
            h.extend_from_slice(&(data.len() as u32).to_le_bytes());
            h.extend_from_slice(&(data.len() as u32).to_le_bytes());
            h.extend_from_slice(&[5, 0, 0, 0]);
            let name = [b'a' + i as u8, b'.', b'j', b'p', b'g'];
            cd.extend_from_slice(b"PK\x01\x02\x14\x03");
            cd.extend_from_slice(&h);
            cd.extend_from_slice(&[0; 10]);
            cd.extend_from_slice(&(d.len() as u32).to_le_bytes());
            cd.extend_from_slice(&name);
            d.extend_from_slice(b"PK\x03\x04");
            d.extend_from_slice(&h);
            d.extend_from_slice(&name);
            d.extend_from_slice(data);
        }
        let at = d.len() as u32;
        d.extend_from_slice(&cd);
        d.extend_from_slice(b"PK\x05\x06\0\0\0\0\x02\0\x02\0");
        d.extend_from_slice(&(cd.len() as u32).to_le_bytes());
        d.extend_from_slice(&at.to_le_bytes());
        d.extend_from_slice(&[0, 0]);
        PURE.with(|n| n.set(0));
        let f = recompress(&d, &Options { threads: 1, ..Options::default() });
        assert!(f.applies().iter().any(|a| a.module == aqr_decompress::JPEG));
        assert_eq!(PURE.with(Cell::get), 2);
        assert!(f.eval(&Native::default()).unwrap() == d);
    }

    /// The DEFLATE stream of a gzip file is deconstructed where its data may archive smaller
    /// than the stream by more than the record, and not where it would not: noise gains
    /// nothing, and is not split for nothing.
    #[test]
    fn flate_where_promising() {
        let gz = |name: &str, d: &[u8], level| {
            let mut z = flate2::GzBuilder::new().filename(name).write(Vec::new(), flate2::Compression::new(level));
            std::io::Write::write_all(&mut z, d).unwrap();
            z.finish().unwrap()
        };
        let deconstructed = |d: &[u8]| {
            DECONSTRUCTED.with(|n| n.set(0));
            let f = recompress(d, &Options::default());
            (DECONSTRUCTED.with(Cell::get), f.applies().len())
        };
        // (named as compressed data, which is not scanned for files)
        let noise = aqr_common::rng::noise(300_000);
        assert_eq!(deconstructed(&gz("noise.mp4", &noise, 6)), (0, 0));
        // DEFLATE at level 1 leaves much to the archive
        let text: Vec<u8> =
            (0..30000).flat_map(|i| format!("line {} of the text\n", i * 7 % 1000).into_bytes()).collect();
        let (n, applies) = deconstructed(&gz("text.txt", &text, 1));
        assert_eq!(n, 1);
        assert!(applies > 0);
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

    /// A slice of a stream that is content is scanned wherever it is, as an input of an
    /// application that is not a content too; one of another stream is not.
    #[test]
    fn content_streams() {
        let text: Vec<u8> = (0..20000).flat_map(|i| format!("line {i} of the text\n").into_bytes()).collect();
        let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::new(6));
        std::io::Write::write_all(&mut z, &text).unwrap();
        let d = [&b"header "[..], &z.finish().unwrap()].concat();
        let opt = Options { smaller: false, ..Options::default() };
        let cx = Cx::new(&d, &opt);
        for content in [false, true] {
            let mut f = Formula::default();
            let data =
                if content { f.push_content("data", Kind::Unknown, d.clone()) } else { f.push("data", d.clone()) };
            f.root = Node::apply(aqr_decompress::X86, vec![], vec![data], d.len() as u64);
            let children = nested(&f, &opt, 0, &cx);
            assert_eq!(children.is_some(), content);
            if let Some(mut children) = children {
                assert_eq!(children.len(), 1);
                let child = children.pop().unwrap().1.unwrap().cut(&d);
                assert_eq!(child.applies().iter().map(|a| a.module).collect::<Vec<_>>(), [FLATE]);
                assert!(child.eval(&Native::default()).unwrap() == d);
            }
        }
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
        place_all(&mut f, vec![Some(child())], vec![], &Options::default());
        let cut = f.streams.iter().find(|s| s.class == "raw").unwrap();
        assert!(cut.data.as_ptr() == at && cut.data == gaps);
        assert_eq!(f.streams.len(), 3);

        place_all(&mut copied, vec![Some(child())], vec![], &Options::default());
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
            streams: vec![aqr_formula::Stream {
                class: "raw".into(),
                kind: Kind::Unknown,
                content: false,
                data: vec![1, 2, 3],
            }],
            root: Node::apply(FLATE, vec![], vec![Node::slice(0, 0, 3), Node::slice(0, 0, 3)], d.len() as u64),
            shared: vec![],
        };
        assert_eq!(finish(fails, &d, &opt, &cx, Default::default(), false).0, raw);
        // without verification, as it is
        let opt = Options { verify: false, ..opt };
        assert_eq!(
            aqr_decompress::decompress(&finish(wrong, &d, &opt, &cx, Default::default(), false).0, 1),
            Err(aqr::Failure::Invalid("output checksum mismatch".into()))
        );
    }

    /// A PSD file of `w` x `h` 16-bit RGB (whose pixels aqr-psd does not code), its composite
    /// `image` (raw), with color mode data `colors` and image resources `res`.
    fn psd_file(w: u32, h: u32, colors: &[u8], res: &[u8], image: &[u8]) -> Vec<u8> {
        let mut d = b"8BPS\0\x01\0\0\0\0\0\0\0\x03".to_vec();
        d.extend_from_slice(&h.to_be_bytes());
        d.extend_from_slice(&w.to_be_bytes());
        d.extend_from_slice(&[0, 16, 0, 3]);
        for part in [colors, res] {
            d.extend_from_slice(&(part.len() as u32).to_be_bytes());
            d.extend_from_slice(part);
        }
        d.extend_from_slice(&[0, 0, 0, 0, 0, 0]);
        d.extend_from_slice(image);
        d
    }

    /// In a file turned down whose recompressor tells what its parts are, pixels and structure
    /// are not looked into (what a scan finds there is there by chance), the rest still is.
    #[test]
    fn scan_by_regions() {
        let (w, h) = (256, 64);
        // free-format MPEG audio headers, one frame apart, in noise its recompressor turns down,
        // and in a resource
        let mp3 = |d: &mut [u8], at: usize| {
            for at in [at, at + 500] {
                d[at..at + 4].copy_from_slice(&[0xff, 0xf3, 0x04, 0xc4]);
            }
        };
        let mut image = aqr_common::rng::noise(6 * w * h);
        mp3(&mut image, 1000);
        let mut res = b"8BIM\x04\x0f\0\0\0\0\x04\0".to_vec();
        res.extend(aqr_common::rng::noise(1024));
        mp3(&mut res, 100);
        // a gzip member where nothing is told: the color mode data
        let text: Vec<u8> = (0..2000).flat_map(|i| format!("line {i} of the text\n").into_bytes()).collect();
        let mut z = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::new(6));
        std::io::Write::write_all(&mut z, &text).unwrap();
        let gz = z.finish().unwrap();
        let d = psd_file(w as u32, h as u32, &gz, &res, &[&[0, 0][..], &image].concat());
        for at in [26 + 4 + gz.len() + 4 + 100, d.len() - image.len() + 1000] {
            assert!(detect::starts(&d[at..]) == Some(Format::Mp3) && extent(Format::Mp3, &d[at..]).is_some());
        }
        TRIED.with(|t| t.borrow_mut().clear());
        let f = recompress(&d, &Options::default());
        let tried = TRIED.with(|t| t.borrow().clone());
        assert!(tried.contains(&Format::Psd), "{tried:?}");
        assert!(!tried.contains(&Format::Mp3), "{tried:?}");
        assert!(tried.contains(&Format::Gzip), "{tried:?}");
        assert!(f.applies().iter().any(|a| a.module == FLATE));
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
            scan_with(d, None, &opt, 0, 0, &Cx::new(d, &opt)).map_or(0, |m| m.f.applies().len())
        };
        assert_eq!(found(&data(1)), 1);
        assert_eq!(found(&data(3)), 0);
    }
}
