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
//! - ar, tar and cpio archives: aqr-bundle, rebuilding the archive from the contents of its
//!   members with their sizes and checksums derived.
//! - Text, a whole file of text bytes or data of a name telling text: aqr-wrt, its words and
//!   markup replaced by codes of a dictionary of its own.
//! - PNG, JPEG, MP3, FLAC/WAV/AIFF, PSD/PSB, PDF, SQLite and FASTA: the recompressor's own formula,
//!   its module applied to its streams, and files it embeds (PDF's DEFLATE streams and images,
//!   PSD's and JPEG's embedded files) applied to theirs (see aqr-decompress for the modules).
//!
//! In whatever formula a recompressor gives, the data that its applications take as it is
//! ([`Options::contents`], by default that of a DEFLATE stream, of gzip, zlib or PDF, and
//! that of an LZ4 stream, of the blocks of a LevelDB table, of the entries of a ZIP archive or
//! of the members of an ar, tar or cpio archive)
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
//! ([`Nest::of`]): pixels, fonts, tables and compressed data are not looked into, text is
//! taken as a whole by aqr-wrt, and data of [`Kind::Format`] is taken as a file of that format
//! from its start ([`Nest::As`]).
//! The contents of an application may be slices of several streams one after the other (a
//! ZIP archive's entries, by kind), each recompressed by its own kind. aqr-zip and aqr-bundle
//! give the entries their kinds by their names ([`kind_by_name`]), as gzip's data is by its name, and
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
//! [`aqr_decompress::decompress`] decompresses. [`compress_tree`] archives several files, a
//! tree of them, each recompressed as the entries of a ZIP archive are.
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

mod archive;
mod bundle;
mod detect;
mod exe;
mod extent;
mod flate;
mod sets;
mod verify;
mod zip;

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

pub use aqr_formula::{Formula, Kind, Module, Node, Region};
pub use archive::{Archived, FileReport, Input, Report, Solid, Source, compress_tree, compress_tree_to};
pub use detect::{Format, detect, kind_by_name};
pub use extent::extent;

use aqr_decompress::{BUNDLE, FLATE, LZ4, Native, SNAPPY, ZIP};
use aqr_formula::Stream;
use aqr_formula::aqr::{self, Encoder, Level, description_bound};
use aqr_formula::probe::{Held, Look, Probe};

#[derive(Debug, Clone)]
pub struct Options {
    /// Threads for the recompressors and modules (the formula does not depend on it).
    pub threads: usize,
    /// Formats nested at most this deep: 1 recompresses the file alone, 2 also the data of a
    /// DEFLATE stream in it (of gzip, zlib or PDF), of an LZ4 stream or of the blocks of a
    /// LevelDB table, and so on (0 recompresses nothing). An ar, tar or cpio archive does not
    /// count: the files in it are as deep as it is.
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
    /// Of an archive of several files ([`compress_tree`]): the most bytes of a non-leaf block
    /// that files share (one stream longer is a block of its own), which a file read alone
    /// may have to decompress.
    pub block: usize,
    /// Of an archive of several files: the most bytes of the files of a set coded against each
    /// other, which a file of it read alone has to decode.
    pub set_bytes: usize,
    /// Of an archive of several files: about the most bytes of files made at once (a file
    /// longer is a batch of its own), which bounds the memory taken.
    pub batch: usize,
}

/// Streams shorter than this are not taken as held by the files archived together ([`Held`]):
/// what they share is found by compressing them solid.
pub(crate) const SHARE_MIN: usize = 256 << 10;

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
    /// pixels, zlib headers in text), and costs the scan its time. Text is taken as a whole by
    /// aqr-wrt.
    pub fn of(self, kind: Kind) -> Option<Nest> {
        match kind {
            Kind::Unknown => Some(self),
            Kind::Text => Some(Nest::As(Format::Text)),
            Kind::Pixels | Kind::Font | Kind::Table | Kind::Compressed => None,
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
            block: 16 << 20,
            set_bytes: 64 << 20,
            batch: 512 << 20,
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

/// The contents of the modules of aqr-decompress: the data of [`FLATE`], [`LZ4`], [`SNAPPY`],
/// [`ZIP`] and [`BUNDLE`], scanned.
pub const CONTENTS: [Content; 5] = [
    Content { module: FLATE, input: 0, nest: Nest::Scan },
    Content { module: LZ4, input: 0, nest: Nest::Scan },
    Content { module: SNAPPY, input: 0, nest: Nest::Scan },
    Content { module: ZIP, input: 0, nest: Nest::Scan },
    Content { module: BUNDLE, input: 0, nest: Nest::Scan },
];

/// What recompressing a file carries along: the encoder sizing formulas, and the bytes that
/// DEFLATE streams may still inflate to ([`Options::expansion`]).
pub(crate) struct Cx {
    enc: Encoder,
    inflate: Cell<u64>,
    /// Where the formulas recompressors make are evaluated beforehand, for [`compress`].
    pre: Option<Arc<verify::Prevalidator>>,
    /// Formulas made beforehand (of [`pure`] formats), by the data (where it is, its length)
    /// and the format.
    ready: RefCell<HashMap<(usize, usize, Format), Option<Formula>>>,
    /// The prevalidation of those taken, by where their data is: put off until it is known
    /// whether they are taken into a set ([`sets`]), whose files are not evaluated alone.
    deferred: RefCell<Vec<(usize, verify::Deferred)>>,
    /// The contents planned to be coded in a set against a parent ([`sets::plan`]), which are
    /// not made alone.
    skip: RefCell<HashSet<sets::Key>>,
    /// For the files of an archive (the contents of a formula 0 formats deep, `archive`), the
    /// most bytes of files of a set ([`Options::set_bytes`]).
    top_sets: Option<usize>,
    /// The files of an archive to make (each its index among the contents, its slice and how),
    /// at once once all are known ([`nested`]).
    later: RefCell<Vec<(usize, Node, Nest)>>,
    /// What the files archived together hold ([`Held`]), for an archive of several.
    held: Option<Arc<Held>>,
    /// The dictionary that the text archived together is coded with (aqr-wrt), for an archive
    /// of several: made of the files that are text, so that text holding other text codes to
    /// what holds the other's code. Else each text has its own.
    words: Option<Arc<aqr_wrt::formula::Shared>>,
    /// The text coded with [`Cx::words`] of data the files archived together hold, by its
    /// length and hash: the text that holds it codes to what holds this, so that it costs
    /// nothing as that data does ([`smaller`]). Shared with the contexts of the files and
    /// contents made apart.
    coded_held: Arc<std::sync::Mutex<HashSet<(usize, u64)>>>,
    /// The text of the files of an archive that others hold as contents, coded once ([`Once`]).
    /// Shared with the contexts of the files made apart.
    once: Arc<Once>,
}

/// Text coded once, for the files of an archive that other files of it hold as the contents of
/// their entries or members: the second to code a text of one takes the formula the first made,
/// kept for it, rather than coding the text again; nor is that formula evaluated beforehand
/// again ([`Once::copied`]).
#[derive(Default)]
pub(crate) struct Once {
    /// By the length and XXH3-128 of the text.
    slots: HashMap<(usize, u128), std::sync::Mutex<Slot>>,
    /// Where the texts given a formula kept are (address and length).
    copies: std::sync::Mutex<HashSet<(usize, usize)>>,
}

enum Slot {
    Empty,
    Kept(Option<Formula>),
    Taken,
}

impl Once {
    /// For the texts of these lengths and XXH3-128s.
    pub(crate) fn of(texts: impl IntoIterator<Item = (usize, u128)>) -> Once {
        let slots = texts.into_iter().map(|k| (k, std::sync::Mutex::new(Slot::Empty))).collect();
        Once { slots, copies: Default::default() }
    }

    /// The formula `code` makes of text `d`, or the one made of the same text before, kept.
    fn code(&self, d: &[u8], code: impl FnOnce() -> Option<Formula>) -> Option<Formula> {
        let slot = match self.slots.keys().any(|k| k.0 == d.len()) {
            true => self.slots.get(&(d.len(), xxhash_rust::xxh3::xxh3_128(d))),
            false => None,
        };
        let Some(slot) = slot else { return code() };
        // (held as it is coded: the second waits for it)
        let mut slot = slot.lock().unwrap();
        match std::mem::replace(&mut *slot, Slot::Taken) {
            Slot::Empty => {
                let f = code();
                *slot = Slot::Kept(f.clone());
                f
            }
            Slot::Kept(f) => {
                self.copies.lock().unwrap().insert((d.as_ptr() as usize, d.len()));
                f
            }
            Slot::Taken => code(),
        }
    }

    /// Whether the formula of text `d` was one kept ([`Once::code`]), evaluated beforehand
    /// already.
    fn copied(&self, d: &[u8]) -> bool {
        self.copies.lock().unwrap().contains(&(d.as_ptr() as usize, d.len()))
    }
}

impl Cx {
    fn new(d: &[u8], opt: &Options) -> Cx {
        Cx::of_len(d.len(), opt)
    }

    /// A context of data of `n` bytes.
    fn of_len(n: usize, opt: &Options) -> Cx {
        let inflate = opt.expansion.saturating_mul(n as u64).saturating_add(EXPANSION_SLACK);
        let mut enc = Encoder::with_threads(opt.level, opt.threads);
        if opt.shared_resources {
            enc.share_resources();
        }
        Cx {
            enc,
            inflate: Cell::new(inflate),
            pre: None,
            ready: Default::default(),
            deferred: Default::default(),
            skip: Default::default(),
            top_sets: None,
            later: Default::default(),
            held: None,
            words: None,
            coded_held: Default::default(),
            once: Default::default(),
        }
    }

    /// The most bytes of files of a set of the contents of a formula `depth` formats deep.
    pub(crate) fn set_limit(&self, depth: u32) -> usize {
        match self.top_sets {
            Some(n) if depth == 0 => n,
            _ => usize::MAX,
        }
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

    /// Whether the files archived together hold `data` ([`Held`]), but for the one `within`
    /// is part of.
    pub(crate) fn held_by_others(&self, data: &[u8], within: &[u8]) -> bool {
        self.held.as_ref().is_some_and(|h| h.holds(data, h.owner(within)))
    }

    /// Codes the text archived with `words` ([`Cx::words`]), its resource sized as nothing.
    pub(crate) fn share_words(&mut self, words: Option<Arc<aqr_wrt::formula::Shared>>) {
        if let Some(w) = &words {
            self.enc.hold_resource(&w.resource);
        }
        self.words = words;
    }

    /// Notes the text coded by formula `f` of text `d` ([`Cx::coded_held`]), where it is coded
    /// with [`Cx::words`] and the files archived together hold `d`.
    fn note_coded(&self, f: &Formula, d: &[u8]) {
        let Some(w) = &self.words else { return };
        let shared = f.applies().iter().any(|a| a.resources.first() == Some(&w.resource));
        if shared && f.streams.len() == 1 && self.held_by_others(d, d) {
            self.coded_held.lock().unwrap().insert(stream_key(&f.streams[0].data));
        }
    }

    /// Whether `data` is text noted by [`Cx::note_coded`].
    fn coded_held(&self, data: &[u8]) -> bool {
        let held = self.coded_held.lock().unwrap();
        !held.is_empty() && held.contains(&stream_key(data))
    }
}

/// `d` by its length and hash, for [`Cx::coded_held`].
fn stream_key(d: &[u8]) -> (usize, u64) {
    (d.len(), xxhash_rust::xxh3::xxh3_64(d))
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
    cx.pre = opt.verify.then(|| Arc::new(verify::Prevalidator::spawn(opt.threads, &[d])));
    let f = recompress_at(d, opt, 0, opt.file, &cx);
    let memo = cx.pre.take().map(verify::Prevalidator::finish_shared).unwrap_or_default();
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
            files: &|_, range: Range<usize>| d.get(range).map(<[u8]>::to_vec),
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
pub(crate) fn made(d: &[u8], opt: &Options, depth: u32, how: Nest, cx: &Cx) -> Option<Made> {
    let text = how == Nest::As(Format::Text) || how == Nest::Scan && detect(d) == Some(Format::Text);
    match how {
        _ if depth >= opt.depth => None,
        Nest::Exact => detect(d).and_then(|format| recompress_as(format, d, opt, depth, cx)).map(Made::from),
        // text as aqr-wrt tells of it alone (not [`smaller`]), so that the same text is taken
        // alike wherever it is; else scanned as other data is, or kept raw where named as text
        _ if text => match recompress_as(Format::Text, d, opt, depth, cx) {
            Some(f) => Some(Made::from(f)),
            None if how == Nest::Scan => {
                scan(d, None, opt, depth, cx).filter(|m| smaller(&m.f, m.gaps.as_deref(), d, opt, cx))
            }
            None => None,
        },
        Nest::Scan => scan(d, None, opt, depth, cx).filter(|m| smaller(&m.f, m.gaps.as_deref(), d, opt, cx)),
        Nest::As(format) => {
            scan(d, Some(format), opt, depth, cx).filter(|m| smaller(&m.f, m.gaps.as_deref(), d, opt, cx))
        }
    }
}

/// A formula of data, its stream 0 perhaps yet to be cut out of the data: the bytes of `gaps`,
/// ranges of the data, one after the other. A scan's formula is so until put in place, where
/// the data between the files found is cut out of the data's own stream if it can be.
pub(crate) struct Made {
    pub f: Formula,
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
    let prepared = ready.is_some();
    let f = match ready {
        Some(f) => f,
        None => pure(format, d, opt.threads, cx.words.as_deref(), &cx.once).unwrap_or_else(|| {
            catch_unwind(AssertUnwindSafe(|| match format {
                Format::Pe => exe::pe(d),
                Format::MachO => exe::macho(d),
                Format::Gzip => flate::gzip(d, contents, opt, depth, cx),
                Format::Zlib => flate::zlib(d, contents, opt, depth, cx),
                Format::Zip => zip::zip(d, opt, depth, cx),
                Format::Ar | Format::Tar | Format::Cpio => bundle::bundle(d),
                _ => unreachable!("a pure format"),
            }))
            .ok()
            .flatten()
        }),
    }?;
    if format == Format::Text {
        cx.note_coded(&f, d);
    }
    // evaluated beforehand while its contents are recompressed, which reads it only (once it is
    // known not to be taken into a set, where made beforehand as a PNG file among contents: it
    // is put off, and does not hold up [`verify::reclaim`] of a large formula, as only PNG
    // files are taken into sets)
    let shared = Arc::new(f);
    if let Some(pre) = &cx.pre {
        match prepared && format == Format::Png {
            true => pre
                .job(shared.clone(), d)
                .into_iter()
                .for_each(|j| cx.deferred.borrow_mut().push((d.as_ptr() as usize, j))),
            false if format == Format::Text && cx.once.copied(d) => {}
            false => pre.submit(shared.clone(), d),
        }
    }
    let mark = cx.deferred.borrow().len();
    // (an ar, tar or cpio archive only bundles files: they are as deep as it is)
    let deeper;
    let opt = match format.is_bundle() {
        true => {
            deeper = Options { depth: opt.depth.saturating_add(1), ..opt.clone() };
            &deeper
        }
        false => opt,
    };
    let children = nested(&shared, opt, depth, cx);
    // (where the data put off is, as the formula may be a copy once reclaimed)
    let bases: Vec<usize> = shared.streams.iter().map(|s| s.data.as_ptr() as usize).collect();
    let mut f = verify::reclaim(shared);
    let mut taken = Vec::new();
    if let Some((mut children, plan)) = children {
        let sets = sets::sets(&f, &mut children, &plan, opt, depth, cx);
        for s in &sets {
            for &m in &s.members {
                if let &Node::Slice { stream, start, .. } = &children[m].0 {
                    taken.push(bases[stream] + start as usize);
                }
            }
        }
        place_all(&mut f, children.into_iter().map(|c| c.1).collect(), sets, opt);
    }
    // the contents' put off, but those taken into a set
    let deferred: Vec<_> = cx.deferred.borrow_mut().drain(mark..).collect();
    if let Some(pre) = &cx.pre {
        deferred.into_iter().filter(|(at, _)| !taken.contains(at)).for_each(|(_, j)| pre.send(j));
    }
    // (text as aqr-wrt tells, see [`made`])
    (format == Format::Text || smaller(&f, None, d, opt, cx)).then_some(f)
}

/// Unless [`Options::smaller`] is off, what tells whether recompressing a stream may pay,
/// for its data `depth` formats deep: at the level of the archive, files looked for as
/// [`Nest::of`] tells (scanned, unless nested formats would go deeper than allowed). An ar,
/// tar or cpio archive is scanned for the files in it, as it gains nothing itself; text is
/// told by its data, as aqr-wrt takes no more than a tenth off it.
pub(crate) fn probe(opt: &Options, depth: u32) -> Option<Probe> {
    let look = |kind| match Nest::Scan.of(kind) {
        None | Some(Nest::As(Format::Text)) => Look::No,
        Some(Nest::As(f)) if !f.is_bundle() => Look::Whole,
        Some(_) => Look::Scan,
    };
    let starts = |d: &[u8]| detect(d).is_some();
    let probe = Probe { level: opt.level, look, starts, scan: files_len };
    opt.smaller.then_some(if depth + 1 < opt.depth { probe } else { Probe::flat(opt.level) })
}

/// Bytes of the files of formats that recompressors take in `d`, as a scan would find them:
/// by their signatures, then their ends ([`extent`]), each from the end of the one before. An
/// ar, tar or cpio archive is not one, but the files in it are; nor is text (see [`probe`]).
fn files_len(d: &[u8]) -> usize {
    let len = |p: usize, f: Option<Format>| {
        f.filter(|f| !f.is_bundle() && *f != Format::Text).and_then(|f| extent(f, &d[p..])).unwrap_or(0)
    };
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
    let smaller = !opt.smaller || {
        let pieces: Option<Vec<&[u8]>> = gaps.map(|gaps| gaps.iter().map(|g| &d[g.clone()]).collect());
        let stand_in = pieces.as_deref().map(|p| (0, p));
        // (the streams the files archived together hold cost nothing)
        let free: Vec<usize> = (0..f.streams.len())
            .filter(|&s| stand_in.is_none_or(|(t, _)| t != s))
            .filter(|&s| {
                // (text coded alike as its copies, whose data was long enough to tell)
                let data = &f.streams[s].data;
                data.len() >= SHARE_MIN && cx.held_by_others(data, d) || cx.coded_held(data)
            })
            .collect();
        cx.enc.smaller_but(f, stand_in, &free, d).expect("slices within their streams")
    };
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
/// where it has any application (as many as fit in [`Options::description`]), and the plan of
/// the sets of them (whose members with a parent have no formula); `None` where none has any
/// and there is no set.
fn nested(f: &Formula, opt: &Options, depth: u32, cx: &Cx) -> Option<(Contents, sets::Plan)> {
    if depth + 1 >= opt.depth {
        return None;
    }
    let (prepared, plan) = prepare(f, opt, depth, cx);
    let mut children = Vec::new();
    contents(&f.root, f, opt, depth, cx, &mut children);
    make_later(f, opt, depth, cx, &mut children);
    // (those not taken: the contents' own nested contents make and take their own)
    let mut ready = cx.ready.borrow_mut();
    prepared.iter().for_each(|k| drop(ready.remove(k)));
    drop(ready);
    let mut skip = cx.skip.borrow_mut();
    plan.children().for_each(|k| _ = skip.remove(&k));
    drop(skip);
    let sizes: Vec<_> = children
        .iter()
        .map(|(slice, child)| match child {
            Some(c) => (slice.len() as usize, description_bound(&c.f.root).saturating_sub(description_bound(slice))),
            None => (0, 0),
        })
        .collect();
    let keep = fit(description_bound(&f.root), &sizes, opt.description);
    if plan.is_empty() && !children.iter().zip(&keep).any(|((_, child), &keep)| keep && child.is_some()) {
        return None;
    }
    let children = children.into_iter().zip(keep).map(|((slice, child), keep)| (slice, child.filter(|_| keep)));
    Some((children.collect(), plan))
}

/// Makes the contents of [`Cx::later`] (the files of an archive) at once, on up to
/// [`Options::threads`] threads, each in a context of its own (with the formula made of it
/// beforehand, if any, and the same prevalidator), into `children`; what is put off of their
/// prevalidation goes to `cx`'s.
fn make_later(f: &Formula, opt: &Options, depth: u32, cx: &Cx, children: &mut Contents) {
    let later = std::mem::take(&mut *cx.later.borrow_mut());
    if later.is_empty() {
        return;
    }
    let data = |n: &Node| {
        let &Node::Slice { stream, start, len } = n else { unreachable!("a slice") };
        &f.streams[stream].data[start as usize..][..len as usize]
    };
    // the formulas made beforehand, by where their data is
    let mut by_at: HashMap<usize, Vec<_>> = HashMap::new();
    for (k, v) in std::mem::take(&mut *cx.ready.borrow_mut()) {
        by_at.entry(k.0).or_default().push((k, v));
    }
    let ready: Vec<std::sync::Mutex<Vec<_>>> = later
        .iter()
        .map(|(_, n, _)| std::sync::Mutex::new(by_at.remove(&(data(n).as_ptr() as usize)).unwrap_or_default()))
        .collect();
    cx.ready.borrow_mut().extend(by_at.into_values().flatten());
    let (pre, held, words, coded_held) = (cx.pre.clone(), cx.held.clone(), cx.words.clone(), cx.coded_held.clone());
    let once = cx.once.clone();
    let made = aqr_common::pool::par_map(later.len(), opt.threads, |i| {
        let (_, n, nest) = &later[i];
        let d = data(n);
        let mut own = Cx::of_len(d.len(), opt);
        own.pre = pre.clone();
        own.held = held.clone();
        own.share_words(words.clone());
        own.coded_held = coded_held.clone();
        own.once = once.clone();
        own.ready.borrow_mut().extend(std::mem::take(&mut *ready[i].lock().unwrap()));
        let child = made(d, opt, depth + 1, *nest, &own).filter(|c| !c.f.applies().is_empty());
        (child, own.deferred.into_inner())
    });
    for ((at, ..), (child, deferred)) in later.into_iter().zip(made) {
        children[at].1 = child;
        cx.deferred.borrow_mut().extend(deferred);
    }
}

/// Contents, each its slice and its formula where it has one ([`nested`]).
type Contents = Vec<(Node, Option<Made>)>;

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
            let s = f.streams.get(stream);
            let d = s.and_then(|s| s.data.get(start as usize..)?.get(..len as usize));
            // (the first of a set's contents with a parent: made in the set; any other alone)
            if d.is_some_and(|d| cx.skip.borrow_mut().remove(&(d.as_ptr() as usize, d.len()))) {
                out.push((n.clone(), None));
                return;
            }
            // (the files of an archive made later, at once)
            if depth == 0
                && cx.top_sets.is_some()
                && let Some(nest) = s.and_then(|s| nest.of(s.kind)).filter(|_| d.is_some())
            {
                cx.later.borrow_mut().push((out.len(), n.clone(), nest));
                out.push((n.clone(), None));
                return;
            }
            let child = s.zip(d).and_then(|(s, d)| made(d, opt, depth + 1, nest.of(s.kind)?, cx));
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
/// little. The formula is the same whatever `threads` is. Text is coded with `words` where
/// given ([`Cx::words`]), and as `once` tells.
fn pure(
    format: Format,
    d: &[u8],
    threads: usize,
    words: Option<&aqr_wrt::formula::Shared>,
    once: &Once,
) -> Option<Option<Formula>> {
    let encode = pure_encode(format)?;
    #[cfg(test)]
    tests::PURE.with(|n| n.set(n.get() + 1));
    let code = || {
        #[cfg(test)]
        if format == Format::Text {
            tests::TEXTS.lock().unwrap().push(d.len());
        }
        catch_unwind(AssertUnwindSafe(|| match (format, words) {
            (Format::Text, Some(w)) => aqr_wrt::formula::encode_with(d, w, threads),
            _ => encode(d, threads),
        }))
        .ok()
        .flatten()
    };
    Some(if format == Format::Text { once.code(d, code) } else { code() })
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
        Format::Fasta => aqr_fasta::formula::encode,
        Format::Text => aqr_wrt::formula::encode,
        _ => return None,
    })
}

/// Makes the formulas of the files of [`pure`] formats among the contents of `f` beforehand,
/// at once (as [`contents`] would take them in turn), into [`Cx::ready`]; returns their keys,
/// and the plan of the sets of the PNG files among them ([`sets::plan`]), whose members with
/// a parent are not made but left out of the contents ([`Cx::skip`]).
fn prepare(f: &Formula, opt: &Options, depth: u32, cx: &Cx) -> (Vec<(usize, usize, Format)>, sets::Plan) {
    let mut jobs = Vec::new();
    pure_contents(&f.root, None, f, opt, &mut jobs);
    if jobs.len() < 2 {
        return Default::default();
    }
    // (PNG files all of their contents, as sets take them)
    let pngs: Vec<_> = jobs.iter().filter(|j| j.0 == Format::Png).filter_map(|j| Some((j.1, j.2?))).collect();
    let plan = sets::plan(&pngs, opt.threads, cx.set_limit(depth));
    let skip: HashSet<sets::Key> = plan.children().collect();
    jobs.retain(|j| !(j.0 == Format::Png && skip.contains(&(j.1.as_ptr() as usize, j.1.len()))));
    cx.skip.borrow_mut().extend(skip);
    let workers = opt.threads.min(jobs.len()).max(1);
    let inner = (opt.threads / workers).max(1);
    let words = cx.words.as_deref();
    let once = &*cx.once;
    let made = sets::par_map(jobs.len(), workers, |i| pure(jobs[i].0, jobs[i].1, inner, words, once).flatten());
    let mut ready = cx.ready.borrow_mut();
    let keys: Vec<_> = jobs.iter().map(|&(format, d, _)| (d.as_ptr() as usize, d.len(), format)).collect();
    for (&k, f) in keys.iter().zip(made) {
        ready.insert(k, f);
    }
    (keys, plan)
}

/// The files of [`pure`] formats below node `n` of `f` that [`contents`] (with `nest` `None`)
/// or [`content_slices`] (with it) would take as such (those of a kind of the format, or that a
/// scan finds at the start), each with its data as it is given its recompressor (to its
/// [`extent`]), and its slice where that is all of the content.
fn pure_contents<'a>(
    n: &Node,
    nest: Option<Nest>,
    f: &'a Formula,
    opt: &Options,
    out: &mut Vec<(Format, &'a [u8], Option<sets::At>)>,
) {
    match (n, nest) {
        (&Node::Slice { stream, .. }, None) if is_content(f, stream) => pure_contents(n, Some(Nest::Scan), f, opt, out),
        (&Node::Slice { stream, start, len }, Some(nest)) => {
            let Some(s) = f.streams.get(stream) else { return };
            let Some(d) = s.data.get(start as usize..).and_then(|d| d.get(..len as usize)) else { return };
            // (a scan takes the file it starts with as one of the format it starts like)
            let format = match nest.of(s.kind) {
                Some(Nest::As(format)) => Some(format),
                Some(Nest::Scan) => detect(d),
                _ => None,
            };
            if let Some(format) = format.filter(|&f| is_pure(f))
                && let Some(n) = extent(format, d).filter(|&n| n > 0)
            {
                out.push((format, &d[..n], (n == d.len()).then_some((stream, start, len))));
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
        /// Whether [`verify::reclaim`] copies every formula (as it does those still evaluated).
        pub(crate) static COPY: Cell<bool> = const { Cell::new(false) };
    }

    /// The lengths of the texts given to aqr-wrt, on any thread.
    pub(crate) static TEXTS: std::sync::Mutex<Vec<usize>> = std::sync::Mutex::new(Vec::new());

    thread_local! {
    }

    /// An ar, tar or cpio archive is worth what the files in it are: a probe counts them, not
    /// the archive, and scans data of its kind.
    #[test]
    fn bundles_probed_by_members() {
        let fixture = |p: &str| std::fs::read(format!("{}/../{p}", env!("CARGO_MANIFEST_DIR"))).unwrap();
        let tar = fixture("aqr-bundle/tests/data/py-gnu.tar");
        assert_eq!(files_len(&tar), 0);
        let jpeg = fixture("aqr-jpeg/tests/data/gradient.jpg");
        let mut with = tar[..1024].to_vec();
        with.extend_from_slice(&jpeg);
        with.extend_from_slice(&tar[1024..]);
        assert_eq!(files_len(&with), jpeg.len());
        let probe = probe(&Options::default(), 0).unwrap();
        assert_eq!((probe.look)(Kind::Format("tar")), Look::Scan);
        assert_eq!((probe.look)(Kind::Format("jpeg")), Look::Whole);
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

    /// A text file, and a ZIP archive of it, archived together: the text is coded once, the
    /// archive's entry taking what was coded of the file.
    #[test]
    fn text_coded_once() {
        let mut t = Vec::new();
        let mut x = 7u64;
        let words = ["alpha", "beta", "gamma", "delta", "epsilon", "zeta", "eta", "theta"];
        while t.len() < 1_000_003 {
            x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            std::io::Write::write_fmt(&mut t, format_args!("{} {} ", words[(x >> 61) as usize], (x >> 40) % 1000)).unwrap();
        }
        t.truncate(1_000_003);
        let zip = stored_zip(&[("t.txt", &t)]);
        let tree = aqr::Tree {
            entries: ["t.txt", "t.zip"]
                .iter()
                .enumerate()
                .map(|(c, name)| aqr::Entry {
                    parent: None,
                    name: name.as_bytes().to_vec(),
                    node: aqr::EntryNode::File(c),
                    extra: Default::default(),
                })
                .collect(),
            ..Default::default()
        };
        let inputs = [Input { name: b"t.txt", data: &t }, Input { name: b"t.zip", data: &zip }];
        let a = compress_tree(&tree, &inputs, &Options { threads: 4, ..Options::default() });
        assert!(a.report.files.iter().all(|f| f.modules.iter().any(|m| m.name == "wrt-recons")), "{:?}", a.report);
        assert!(a.report.unverified.is_empty());
        assert_eq!(TEXTS.lock().unwrap().iter().filter(|&&n| n == t.len()).count(), 1);
    }

    /// A stored ZIP archive of `files` (name, data).
    fn stored_zip(files: &[(&str, &[u8])]) -> Vec<u8> {
        let (mut d, mut cd) = (Vec::new(), Vec::new());
        for &(name, data) in files {
            let mut h = vec![20, 0, 0, 0, 0, 0, 0, 0, 0, 0];
            h.extend_from_slice(&crc32fast::hash(data).to_le_bytes());
            h.extend_from_slice(&(data.len() as u32).to_le_bytes());
            h.extend_from_slice(&(data.len() as u32).to_le_bytes());
            h.extend_from_slice(&(name.len() as u16).to_le_bytes());
            h.extend_from_slice(&[0, 0]);
            cd.extend_from_slice(b"PK\x01\x02\x14\x03");
            cd.extend_from_slice(&h);
            cd.extend_from_slice(&[0; 10]);
            cd.extend_from_slice(&(d.len() as u32).to_le_bytes());
            cd.extend_from_slice(name.as_bytes());
            d.extend_from_slice(b"PK\x03\x04");
            d.extend_from_slice(&h);
            d.extend_from_slice(name.as_bytes());
            d.extend_from_slice(data);
        }
        let (at, n) = (d.len() as u32, files.len() as u16);
        d.extend_from_slice(&cd);
        d.extend_from_slice(b"PK\x05\x06\0\0\0\0");
        d.extend_from_slice(&n.to_le_bytes());
        d.extend_from_slice(&n.to_le_bytes());
        d.extend_from_slice(&(cd.len() as u32).to_le_bytes());
        d.extend_from_slice(&at.to_le_bytes());
        d.extend_from_slice(&[0, 0]);
        d
    }

    /// A gray PNG file of a gradient of `w` x `h`, with the pixels of `rect` (x, y, width,
    /// height) changed.
    fn variant(w: usize, h: usize, rect: Option<(usize, usize, usize, usize)>) -> Vec<u8> {
        use std::io::Write;
        let mut data = Vec::new();
        for y in 0..h {
            data.push(0);
            for x in 0..w {
                let inside =
                    rect.is_some_and(|(rx, ry, rw, rh)| (rx..rx + rw).contains(&x) && (ry..ry + rh).contains(&y));
                data.push(if inside { ((x * 7) ^ (y * 3)) as u8 } else { (x + 2 * y) as u8 / 3 });
            }
        }
        let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::new(6));
        z.write_all(&data).unwrap();
        let mut ihdr = (w as u32).to_be_bytes().to_vec();
        ihdr.extend_from_slice(&(h as u32).to_be_bytes());
        ihdr.extend_from_slice(&[8, 0, 0, 0, 0]);
        let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
        for (kind, d) in [(b"IHDR", ihdr), (b"IDAT", z.finish().unwrap()), (b"IEND", vec![])] {
            out.extend_from_slice(&(d.len() as u32).to_be_bytes());
            let start = out.len();
            out.extend_from_slice(kind);
            out.extend_from_slice(&d);
            out.extend_from_slice(&crc32fast::hash(&out[start..]).to_be_bytes());
        }
        out
    }

    /// The PNG files of a set are coded alone only where they have no parent: the others are
    /// coded against theirs only.
    #[test]
    fn set_members_not_coded_alone() {
        let files = [
            variant(200, 150, None),
            variant(200, 150, Some((20, 30, 40, 50))),
            variant(200, 150, Some((90, 10, 30, 90))),
        ];
        let d = stored_zip(&[("a.png", &files[0]), ("b.png", &files[1]), ("c.png", &files[2])]);
        PURE.with(|n| n.set(0));
        let f = recompress(&d, &Options { threads: 1, ..Options::default() });
        assert_eq!(f.shared.len(), 1);
        assert_eq!(f.shared[0].sizes.len(), 3);
        assert_eq!(PURE.with(Cell::get), 1);
        assert!(f.eval(&Native::default()).unwrap() == d);
    }

    /// The sets planned are made where the formula of the contents is a copy once reclaimed
    /// from the prevalidator (as one is that it still evaluates).
    #[test]
    fn sets_of_copies() {
        let files = [
            variant(200, 150, None),
            variant(200, 150, Some((20, 30, 40, 50))),
            variant(200, 150, Some((90, 10, 30, 90))),
        ];
        let d = stored_zip(&[("a.png", &files[0]), ("b.png", &files[1]), ("c.png", &files[2])]);
        COPY.with(|c| c.set(true));
        let (c, f) = compress_formula(&d, &Options { threads: 2, ..Options::default() });
        COPY.with(|c| c.set(false));
        assert_eq!(f.shared.len(), 1);
        assert!(aqr_decompress::decompress(&c, 1).unwrap() == d);
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
        // (of no words, which aqr-wrt would take)
        let text: Vec<u8> = (0..20000).flat_map(|i| format!("{i}: {}\n", i * 7).into_bytes()).collect();
        let mut z = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::new(6));
        std::io::Write::write_all(&mut z, &text).unwrap();
        let d = z.finish().unwrap();
        let opt = Options { smaller: false, ..Options::default() };
        let mut cx = Cx::new(&d, &opt);
        cx.pre = Some(Arc::new(verify::Prevalidator::spawn(2, &[&d])));
        let f = recompress_at(&d, &opt, 0, opt.file, &cx);
        let memo = verify::Prevalidator::finish_shared(cx.pre.take().unwrap());
        assert!(!f.applies().is_empty());
        // the DEFLATE stream is not kept again, but known to be the file's
        assert_eq!(memo.bytes(), 0);
        let c = cx.enc.encode(&f, Some(aqr::checksum(&d))).unwrap();
        let decode = |c: &[u8], memo: verify::Memo| {
            let m = verify::Cached {
                native: Native { threads: 1 },
                memo: memo.into(),
                hits: Default::default(),
                files: &|_, range: Range<usize>| d.get(range).map(<[u8]>::to_vec),
            };
            let out = aqr::decode(c, &aqr::natives(&aqr_decompress::MODULES, &m));
            (out, m.hits.into_inner().unwrap())
        };
        let other = compress(&d[..d.len() - 1], &Options { verify: false, ..opt.clone() });
        // another archive's applications are not in it
        let (memo, hits) = {
            let m = verify::Cached {
                native: Native { threads: 1 },
                memo: memo.into(),
                hits: Default::default(),
                files: &|_, range: Range<usize>| d.get(range).map(<[u8]>::to_vec),
            };
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
        // (of no words, which aqr-wrt would take)
        let text: Vec<u8> = (0..20000).flat_map(|i| format!("{i}: {}\n", i * 7).into_bytes()).collect();
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
            if let Some((mut children, _)) = children {
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
