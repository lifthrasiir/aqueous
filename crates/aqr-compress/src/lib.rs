//! Recompression of a file into an entry formula (docs/xform-design.md §4.2, §6): [`detect`] picks
//! the recompressor by the magic bytes of a file, and [`recompress`] runs it on each file found,
//! giving a [`Formula`] rather than bytes.
//!
//! - PE images, Mach-O images (thin or universal) and ELF images: aqr-exe's x86 disfilter or
//!   ARM64 filter applied to each executable section, over its streams; the rest of the file is
//!   sliced in between, a content stream (below). Of an NSIS installer, the files its data
//!   stores as they are (`nsis`), each a content of its format, as deep as the installer.
//! - gzip and zlib: the container sliced around aqr-flate on the DEFLATE stream (data and
//!   record). Below level 4, DEFLATE streams (theirs and ZIP archives') are not rebuilt but
//!   kept as they are, each a slice of its own, and the files archived along with them that
//!   hold their data are made of them by aqr-flate's `flate-inflate` (`inflated`).
//! - mozlz4 files and LZ4 frames: the container sliced around aqr-lz4 on the data.
//! - `.lzma` files: the header sliced around aqr-lzma on the data and record of the stream.
//! - `.xz` files: the container sliced around aqr-lzma on the data and records of the LZMA2
//!   streams of its blocks. (These and 7z archives from level 4: `format_on`.)
//! - lzop files: the headers sliced around aqr-lzo on the data and records of the streams.
//! - Streams of Apple's LZFSE and LZVN blocks (`bvx2`, `bvxn`; asset catalogs hold them):
//!   aqr-bvx on the data and record of the stream (from level 9).
//! - bzip2 streams, one after another: aqr-bz2 on their data, what follows sliced.
//! - Zstandard files: kept as they are (not rebuilt); the files of the same batch that are
//!   their content, or a member of the tar archive it is, taken from their frames, which the
//!   archive's Patch instructions decompress (`zst`, aqr-zstd).
//! - LevelDB tables: aqr-snappy on the contents of their blocks.
//! - ZIP archives: aqr-zip, rebuilding the archive from the contents of its entries (the
//!   DEFLATE streams by aqr-flate) with its headers derived.
//! - 7z archives: aqr-7z, rebuilding the folders of LZMA, LZMA2 or Copy alone from the contents
//!   of the files they hold (by aqr-lzma), the rest of the archive kept; from level 4.
//! - git packs: aqr-git, rebuilding the pack from the data of its objects (the zlib streams by
//!   aqr-flate) with its headers, checksums and trailer derived; the blobs' data is found again
//!   where the files checked out are archived along with it.
//! - HDF5 files: aqr-hdf5, the deflated chunks of their datasets rebuilt by aqr-flate from
//!   their data, the rest of the file sliced around them.
//! - ar, tar and cpio archives: aqr-bundle, rebuilding the archive from the contents of its
//!   members with their sizes and checksums derived.
//! - Electron's asar archives: aqr-asar, rebuilding the archive from the data after its header,
//!   the files' sizes, offsets and SHA-256 integrity derived.
//! - ISO 9660 and UDF images: aqr-iso, the image cut into its files' contents and the rest.
//! - WIM files of LZX: aqr-wim, the resources' data, the LZX chunks rebuilt by aqr-lzx.
//! - Text, a whole file of text bytes or data of a name telling text: JSON lines by aqr-jsonl,
//!   their values stored by key path, or a delimited table (CSV, TSV, USV and the like) by
//!   aqr-xsv, its fields stored column by column, where either takes it; else aqr-wrt, its
//!   words and markup replaced by codes of a dictionary of its own.
//! - Data in which a scan finds no file (the rest of an executable, data of no format):
//!   aqr-infer-recs, its tables of records and chains of entries transposed by their fields,
//!   where a quick estimate tells they may save 5% at least and they do.
//! - QuickTime and MXF files of ProRes frames: aqr-prores, the frames' coefficients under a
//!   context model with the frame before, in chunks of about 32 MiB.
//! - Ogg files of Vorbis streams: aqr-ogg, rebuilding the pages from the streams' packets with
//!   their lacing and CRCs derived, the Vorbis audio packets by aqr-vorbis.
//! - PNG, GIF, JPEG, MP3, FLAC/WAV/AIFF, PSD/PSB, Clip Studio Paint, DDS, PDF, SQLite, FASTA, DEX, `resources.arsc`, FITS,
//!   PDB (its container and modules' symbols) and Wavefront OBJ (by its text, not its name): the recompressor's own formula, its module
//!   applied to its streams, and files it embeds (PDF's DEFLATE streams and images, PSD's and
//!   JPEG's embedded files, Clip Studio Paint's database) applied to theirs (see aqr-decompress
//!   for the modules).
//!
//! In whatever formula a recompressor gives, the data as it is ([`Stream::content`]) is
//! recompressed in turn, up to [`Options::depth`] formats deep, and its formula put in place
//! of the data: the slices of the streams that a recompressor gives as such (the rest of an
//! executable, around its code; the files of an image), and of those that its applications
//! take as such ([`Module::contents`]: the data of a DEFLATE stream, of gzip, zlib or PDF, of
//! an LZ4, LZMA or LZMA2 stream, of the blocks of a LevelDB table, of the entries of a ZIP
//! archive, of the members of an ar, tar or cpio archive, of the files of an asar archive;
//! [`Formula::mark_contents`]), wherever they are.
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
//! ZIP archive's entries, by kind), each recompressed by its own kind. aqr-zip, aqr-bundle and
//! aqr-asar give the entries their kinds by their names ([`kind_by_name`]), as gzip's data is by its name, and
//! aqr-pdf the streams of a PDF file by what they are.
//!
//! Contents that are PNG files of the same geometry and alike (variants of a picture, with
//! layers shown or hidden) are coded against each other, as one set split into the files
//! (`sets`), where that is smaller than coding them alone.
//!
//! A file that a scan finds but its recompressor turns down is looked into by its parts where
//! the recompressor tells them ([`Region`]; aqr-psd does): the parts of a format are taken as
//! files of it, the parts of the other kinds are not looked into, and only the bytes no part
//! tells of (and parts of unknown kind) are scanned. Else the scan goes on from its next byte.
//!
//! What DEFLATE streams and streams of LZFSE and LZVN blocks inflate to, at every depth
//! together, is kept within [`Options::expansion`] times the file (and 64 MiB): past it,
//! streams are kept as they are, so that nested archives (42.zip) and bombs cost no more than
//! that.
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

mod aqueous;
mod archive;
mod bundle;
mod chunked;
mod detect;
mod dwarf;
mod exe;
mod extent;
mod flate;
mod formats;
mod git;
mod hdf5;
mod inflated;
pub mod mapped;
mod memory;
pub use memory::{DEFAULT_BOUND, Fallback, bound_text, fallbacks};
mod nsis;
mod pak;
mod progress;
mod seal;
mod sets;
pub mod spill;
mod verify;
mod zip;
mod zst;

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::io::{self, Seek, Write};
use std::ops::{Deref, Range};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

pub use aqr_formula::{Formula, Kind, Module, Node, Region};
pub use archive::{
    Appending, Archived, FileReport, Input, Output, Plain, Report, Solid, Source, Stop, Stopped, compress_tree,
    compress_tree_into, compress_tree_to,
};
pub use detect::{Format, detect, kind_by_name};
pub use extent::extent;
pub use progress::Event;

use aqr_decompress::Native;
use aqr_formula::aqr::{self, Encoder, Level, description_bound};
use aqr_formula::probe::{Held, Look, Probe};
use aqr_formula::{Bytes, Stream};

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
    /// (R8). [`recompress`] does not verify: whoever takes its formula does. Nor is anything
    /// verified below level 2, where nothing is recompressed ([`Options::level`]).
    pub verify: bool,
    /// How the file itself is recompressed.
    pub file: Nest,
    /// The most bytes of description a formula may take ([`aqr::description_bound`]): where
    /// the files found in data, or the contents of a file, would take more, those with the
    /// fewest bytes per byte of their description are kept raw. By default no limit.
    pub description: usize,
    /// The level of the archive of [`compress`] ([`aqr::Encoder`]), and of the formulas sized
    /// for [`Options::smaller`]; and, by its [`Level::scale`], how hard data is recompressed:
    /// at 0 and 1 not at all (whatever [`Options::depth`]: at 0 every block uncompressed, at 1
    /// zstd alone), from 7 on DEFLATE as [`Options::flate`] tells and with no `probe`.
    pub level: Level,
    /// Keep a file raw, or the data where files are found, unless its formula archives
    /// smaller than it does raw ([`aqr::Encoder::smaller`]).
    pub smaller: bool,
    /// The most bytes of data that DEFLATE streams (of gzip, zlib and ZIP) and streams of LZFSE
    /// and LZVN blocks inflate to, at every depth together, as a multiple of the file, on top of
    /// [`EXPANSION_SLACK`]: the streams past it are kept as they are.
    pub expansion: u64,
    /// Size formulas without their resources ([`aqr::Encoder::share_resources`]), as for an
    /// archive of many files sharing them: for measuring what they come to in the limit.
    pub shared_resources: bool,
    /// Size formulas without their module blocks ([`aqr::Encoder::share_modules`]), as an
    /// archive of several files does ([`compress_tree`]), whose modules are written once.
    pub shared_modules: bool,
    /// How each module is written into the archive ([`aqr::Encoder::with_modules`]): by default
    /// its Wasm module, as aqr-modules built it ([`aqr_decompress::wasm_of`]); a self-extracting
    /// archive names those its program carries by their hashes alone ([`aqr::Wasm::Registered`]).
    pub modules: aqr::WasmOf,
    /// Of an archive of several files ([`compress_tree`]): the most bytes of a non-leaf block
    /// that files share (one stream longer is a block of its own), which a file read alone
    /// may have to decompress.
    pub block: usize,
    /// Of an archive of several files: compress the blocks alike of a batch (many of a class
    /// and kind of stream, small ones) with a dictionary trained for them where it pays
    /// ([`aqr::Encoder::with_dictionaries`]). Off by default; it matters only with blocks of a
    /// few MiB or less ([`Options::block`]).
    pub dictionaries: bool,
    /// Begin the archive with a size hint block to be filled in, as the payload of an
    /// encrypted archive has to ([`aqr::envelope::Sealer`], which fills it in).
    pub size_hint: bool,
    /// Give the archive a mirror block of each block whose damage would lose more than a file
    /// (spec, "Mirror block"; [`aqr::Mirroring`]), for it to be repaired: as far from it as an
    /// encrypted archive's payload asks where [`Options::size_hint`] (which it has to have).
    pub mirrors: bool,
    /// Of an archive of several files: the most bytes of the files of a set coded against each
    /// other, which a file of it read alone has to decode.
    pub set_bytes: usize,
    /// Of an archive of several files: about the most bytes of files made at once (a file
    /// longer is a batch of its own), which bounds the memory taken.
    pub batch: usize,
    /// Of an archive of several files: about the least bytes of a segment of it (spec,
    /// "Segments and entries"), which is closed by its metadata block after the batch that
    /// makes it so long (a batch longer makes a segment longer), so that an archive cut short
    /// or damaged loses no more than a segment's names; the next takes nothing of it (its
    /// modules and dictionaries written again, a file of the same content as one of it made
    /// again). None (0): one segment.
    pub segment: usize,
    /// Where large buffers of streams are kept rather than on the heap: the path the names of
    /// [`spill::Spill`]'s files extend (made there and gone once their buffers are dropped;
    /// beside the output by aqr, named after it, as a temporary directory may be on another
    /// drive, but where that is a drive of the network): the system writes them out as memory
    /// runs short. None by default.
    pub spill: Option<std::path::PathBuf>,
    /// The most bytes that an application of a module may have as its minimal bound (its memory
    /// limit and the sum of its task limits; spec, "Module execution"): a recompression whose
    /// formula has a larger one is not used, its data kept as it would be without it
    /// ([`memory::fallbacks`] tells). None, the default: 256 MiB ([`memory::DEFAULT_BOUND`]),
    /// or more for a module with an exception (`memory`); decoders support up to 512 MiB.
    pub application_bound: Option<u64>,
    /// Of an archive of several files: the journal point (spec, "`journal`") the last segment of
    /// the writing names: a time in milliseconds since the epoch (UTC) and a label. None by
    /// default; not told of a writing stopped.
    pub journal: Option<(u64, String)>,
}

/// The bytes of a segment of an archive of several files by default ([`Options::segment`]).
pub const SEGMENT: usize = 256 << 20;

/// Streams shorter than this are not taken as held by the files archived together ([`Held`]):
/// what they share is found by compressing them solid.
pub(crate) const SHARE_MIN: usize = 256 << 10;

/// Bytes that DEFLATE, LZFSE and LZVN streams may inflate to on top of [`Options::expansion`]
/// times the file.
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

impl Options {
    /// The aqr-flate profile that DEFLATE streams (of gzip, zlib and ZIP) are rebuilt with, and
    /// whether it tries zlib emulation (at every level): up to level 6, the default profile;
    /// from 7 on, the adaptive one with chain rules and zlib emulation (enwik8 by
    /// Info-ZIP 3.0 `-9`, 10 threads of a Ryzen 7 9700X: a record of 0.41% of the data rather
    /// than 2.6% with chain rules alone, split at 66 MB/s rather than 163, joined at 147 MB/s
    /// rather than 302; 2.8 KB with zlib emulation, split at 54 MB/s, joined at 165 MB/s, but at
    /// 25 MB/s on a thread: `enwik9.zip` alone some 40 s of decoding). Streams not of zlib's
    /// cost nothing more: detection turns them down at once.
    pub fn flate(&self) -> (u16, bool) {
        match self.level.scale() {
            ..7 => (aqr_flate::DEFAULT_PROFILE, true),
            _ => (aqr_flate::ADAPTIVE, true),
        }
    }

    /// Whether data is recompressed at all ([`Options::level`]).
    pub(crate) fn recompresses(&self) -> bool {
        !matches!(self.level, Level::Of(0 | 1))
    }
}

impl Default for Options {
    fn default() -> Options {
        Options {
            threads: 1,
            depth: 3,
            verify: true,
            file: Nest::Scan,
            description: usize::MAX,
            level: Level::MID,
            smaller: true,
            expansion: 32,
            shared_resources: false,
            shared_modules: false,
            modules: aqr_decompress::wasm_of,
            block: 16 << 20,
            dictionaries: false,
            size_hint: false,
            mirrors: false,
            set_bytes: 64 << 20,
            batch: 512 << 20,
            segment: SEGMENT,
            spill: None,
            application_bound: None,
            journal: None,
        }
    }
}

/// What recompressing a file carries along: the encoder sizing formulas, and the bytes that
/// DEFLATE, LZFSE and LZVN streams may still inflate to ([`Options::expansion`]).
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
    /// Where the contents made apart of the formula of this data are sealed, if they are
    /// ([`seal`]): those of a formula whose formulas above all make their files apart, of a
    /// file archived alone. Taken by what may use it: [`recompress_with`], for [`nested`] only
    /// where its formula makes its files apart, and [`find`], only for a file that is all of
    /// the data (else the formula of the scan, or of what holds it, is compared with its data,
    /// [`smaller`], which reads the streams of the formulas sealed: they are no longer at hand).
    sealing: RefCell<Option<seal::Sealing>>,
    /// Whether [`make_later`] had the applications of contents of data not made yet evaluated
    /// as it made them ([`recompress_with`] evaluates the formula otherwise).
    parts_checked: std::cell::Cell<bool>,
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
    /// The DEFLATE streams of the files archived together, kept as they are, that files holding
    /// their data are made of ([`inflated`]), for an archive of several below level 4.
    bodies: Option<Arc<inflated::Bodies>>,
    /// The remaps of the ARM64 and x86 images of the Mach-O and ELF files archived together
    /// ([`exe::plan`]).
    remaps: Option<Arc<exe::Remaps>>,
    /// The dictionary that the text archived together is coded with (aqr-wrt), for an archive
    /// of several: made of the files that are text, so that text holding other text codes to
    /// what holds the other's code; and for the contents of a formula of JSON lines, its
    /// segments' text ([`segment_words`]). Else each text has its own.
    words: RefCell<Option<Arc<aqr_wrt::formula::Shared>>>,
    /// The text coded with [`Cx::words`] of data the files archived together hold, by its
    /// length and hash: the text that holds it codes to what holds this, so that it costs
    /// nothing as that data does ([`smaller`]). So are the streams of the members of an ar, tar
    /// or cpio archive that they hold ([`Cx::note_bundled`]), and those of the formula of any
    /// data they hold ([`Cx::note_held`]). A context made apart (a file of an archive, a
    /// content made on a worker) starts with those noted before it was let go ([`Apart`]), and
    /// what it notes is taken back once it and those made beside it are done
    /// ([`Cx::adopt_notes`]): not shared as they are made, so that what one sees of another
    /// does not depend on which of them a worker took first (the archive on the threads).
    coded_held: RefCell<HashSet<(usize, u64)>>,
    /// The text of the files of an archive that others hold as contents, coded once ([`Once`]).
    /// Shared with the contexts of the files made apart.
    once: Arc<Once>,
    /// The files of an archive (the contents of a formula 0 formats deep), to tell as they are
    /// made. Not shared with the contexts of the files.
    files: Option<Arc<progress::Files>>,
    /// The data files of unknown archive formats to take apart, by where their data is
    /// ([`pak::plan`]). Shared with the contexts of the files.
    paks: Option<Arc<pak::Placed>>,
}

/// Text coded once, for the files of an archive that other files of it hold as the contents of
/// their entries or members: the second to code a text of one takes the formula the first made,
/// kept for it, rather than coding the text again; nor is that formula evaluated beforehand
/// again ([`Once::evaluated`]). Nor is the first's where the text is large: what it would keep
/// is only that its output is the file, which decoding the archive takes for one of the two
/// alike applications only, reading it as much as running the module gives; it would only
/// hold the output while the files are recompressed (10gb: `enwik9`, 1 GB).
#[derive(Default)]
pub(crate) struct Once {
    /// By the length and XXH3-128 of the text.
    slots: HashMap<(usize, u128), std::sync::Mutex<Slot>>,
    /// Where the texts whose formulas are not evaluated beforehand are (address and length).
    copies: std::sync::Mutex<HashSet<(usize, usize)>>,
}

enum Slot {
    Empty,
    Kept(Option<Formula>),
    Taken,
}

impl Once {
    /// Texts longer than this are not evaluated beforehand ([`Once::evaluated`]).
    const LARGE: usize = 64 << 20;

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
                let mut f = code();
                // (kept without a copy of its streams)
                f.iter_mut().flat_map(|f| &mut f.streams).for_each(|s| s.data.share());
                *slot = Slot::Kept(f.clone());
                if d.len() > Self::LARGE {
                    self.copies.lock().unwrap().insert((d.as_ptr() as usize, d.len()));
                }
                f
            }
            Slot::Kept(f) => {
                self.copies.lock().unwrap().insert((d.as_ptr() as usize, d.len()));
                f
            }
            Slot::Taken => code(),
        }
    }

    /// Whether the formula of text `d` ([`Once::code`]) is to be evaluated beforehand.
    fn evaluated(&self, d: &[u8]) -> bool {
        !self.copies.lock().unwrap().contains(&(d.as_ptr() as usize, d.len()))
    }
}

/// `opt`, but [`Options::verify`] only where data is recompressed ([`Options::recompresses`]):
/// below level 2 no module is applied, and what Zstandard makes of the data in memory is
/// trusted.
pub(crate) fn verifying(opt: &Options) -> Options {
    Options { verify: opt.verify && opt.recompresses(), ..opt.clone() }
}

/// The encoder of formulas made with `opt`.
pub(crate) fn encoder(opt: &Options) -> Encoder {
    let mut enc = Encoder::with_threads(opt.level, opt.threads)
        .with_modules(opt.modules)
        .with_dictionaries(opt.dictionaries)
        .with_size_hint(opt.size_hint);
    if opt.shared_resources {
        enc.share_resources();
    }
    if opt.shared_modules {
        enc.share_modules();
    }
    enc
}

impl Cx {
    fn new(d: &[u8], opt: &Options) -> Cx {
        Cx::of_len(d.len(), opt)
    }

    /// A context of data of `n` bytes.
    fn of_len(n: usize, opt: &Options) -> Cx {
        let inflate = opt.expansion.saturating_mul(n as u64).saturating_add(EXPANSION_SLACK);
        Cx {
            enc: encoder(opt),
            inflate: Cell::new(inflate),
            pre: None,
            ready: Default::default(),
            deferred: Default::default(),
            sealing: Default::default(),
            parts_checked: Default::default(),
            skip: Default::default(),
            top_sets: None,
            later: Default::default(),
            held: None,
            bodies: None,
            remaps: None,
            words: Default::default(),
            coded_held: Default::default(),
            once: Default::default(),
            files: None,
            paks: None,
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

    /// Codes the text archived with `words` ([`Cx::words`]), their resources sized as nothing.
    pub(crate) fn share_words(&self, words: Option<Arc<aqr_wrt::formula::Shared>>) {
        for r in words.iter().flat_map(|w| w.resources()) {
            self.enc.hold_resource(r);
        }
        *self.words.borrow_mut() = words;
    }

    /// Notes the text coded by formula `f` of text `d` ([`Cx::coded_held`]), where it is coded
    /// with [`Cx::words`] and the files archived together hold `d`.
    fn note_coded(&self, f: &Formula, d: &[u8]) {
        let words = self.words.borrow();
        let Some(w) = words.as_deref() else { return };
        let shared = f.applies().iter().any(|a| a.resources.first().is_some_and(|r| w.resources().any(|w| w == r)));
        if shared && f.streams.len() == 1 && self.held_by_others(d, d) {
            self.coded_held.borrow_mut().insert(stream_key(&f.streams[0].data));
        }
    }

    /// Notes the streams of formula `f` of an ar, tar or cpio archive that the files archived
    /// together hold ([`Cx::coded_held`]), but its own (its headers): those of its members, which
    /// are the files', so that what holds the archive costs nothing for them either.
    fn note_bundled(&self, f: &Formula) {
        self.note_streams(f, |s| !bundle::own_class(&s.class));
    }

    /// Notes the streams of formula `f` of data that the files archived together hold
    /// ([`Cx::coded_held`]): the file that holds it has the same formula, whose streams are
    /// stored once (as text is coded alike, [`Cx::note_coded`]), so that what holds the data
    /// (a tar archive the files do not hold all of, say) costs nothing for them either.
    fn note_held(&self, f: &Formula) {
        self.note_streams(f, |_| true);
    }

    fn note_streams(&self, f: &Formula, take: impl Fn(&aqr_formula::Stream) -> bool) {
        let mut held = self.coded_held.borrow_mut();
        for s in f.streams.iter().filter(|s| take(s) && !s.data.is_empty()) {
            held.insert(stream_key(&s.data));
        }
    }

    /// Whether `data` is a stream noted by [`Cx::note_coded`], [`Cx::note_bundled`] or
    /// [`Cx::note_held`].
    fn coded_held(&self, data: &[u8]) -> bool {
        let held = self.coded_held.borrow();
        !held.is_empty() && held.contains(&stream_key(data))
    }

    /// Takes the notes of a context made apart ([`Cx::coded_held`]), once it and those made
    /// beside it are done.
    fn adopt_notes(&self, notes: HashSet<(usize, u64)>) {
        self.coded_held.borrow_mut().extend(notes);
    }
}

/// `d` by its length and hash, for [`Cx::coded_held`].
fn stream_key(d: &[u8]) -> (usize, u64) {
    (d.len(), xxhash_rust::xxh3::xxh3_64(d))
}

/// `d` as a formula, with the recompressor for its format if any takes it.
pub fn recompress(d: &[u8], opt: &Options) -> Formula {
    recompress_at(d, opt, 0, Kind::Unknown, &Cx::new(d, opt))
}

/// Declares the memory limits of `f`'s applications as the compressor does without having run
/// them (from their modules' plans): for tests of formulas made apart.
#[doc(hidden)]
pub fn declare_memory(f: &mut Formula) {
    memory::declare(f, &Native { threads: 1 }, &memory::Limits::default());
}

/// `d` as an archive, with the checksum of `d`.
pub fn compress(d: &[u8], opt: &Options) -> Vec<u8> {
    let mut c = Vec::new();
    compress_with(d, Kind::Unknown, opt, false, None, &mut c).expect("written to memory");
    c
}

/// [`compress`], with the formula the archive holds: that of `d` raw where it was not verified.
pub fn compress_formula(d: &[u8], opt: &Options) -> (Vec<u8>, Formula) {
    compress_formula_as(d, Kind::Unknown, opt)
}

/// [`compress_formula`] of a file of `kind`, as its name tells ([`kind_by_name`]), as a file in
/// an archive of many is taken.
pub fn compress_formula_as(d: &[u8], kind: Kind, opt: &Options) -> (Vec<u8>, Formula) {
    let mut c = Vec::new();
    let (_, f) = compress_with(d, kind, opt, true, None, &mut c).expect("written to memory");
    (c, f.expect("kept"))
}

/// [`compress_formula_as`] written to `out` as it is made, of the bytes written; telling `tell`
/// how far it is as [`compress_tree_to`] does, of `d` as content 0 in a batch of its own (the
/// compression told only once it is encoded, and once it is verified). Fails where `out` does.
pub fn compress_formula_to(
    d: &[u8],
    kind: Kind,
    opt: &Options,
    out: &mut dyn Sink,
    tell: &(dyn Fn(Event) + Sync),
) -> io::Result<(usize, Formula)> {
    let (tracker, notes) = progress::Tracker::new(vec![d.len() as u64]);
    let (n, f) = std::thread::scope(|s| {
        s.spawn(|| tracker.run(tell));
        // (the notes gone with it, the tracker done)
        compress_with(d, kind, opt, true, Some(notes), out)
    })?;
    Ok((n, f.expect("kept")))
}

/// Where [`compress_formula_to`] writes an archive: read back to be verified, and written again
/// from the start where it does not give the file back.
pub trait Sink: Write {
    /// The bytes written so far.
    fn written(&mut self) -> io::Result<Box<dyn Deref<Target = [u8]> + '_>>;

    /// Drops the bytes written, to be written again from the start.
    fn restart(&mut self) -> io::Result<()>;

    /// Whether the bytes [`Sink::written`] gives are a mapping, read only, of a file: whose
    /// pages may be let go of as they are read ([`aqr::Archive::let_go_read`]).
    fn mapped(&self) -> bool {
        false
    }

    /// Where nothing is written yet and bytes can be written at offsets from its start (a file):
    /// how, for blocks to be written as they are made and the formula block before them after
    /// ([`direct`]). The sink is then written to only through it, until [`Sink::placed`].
    fn place(&mut self) -> Option<Arc<Placing>> {
        None
    }

    /// Has the sink go on after the `len` bytes written through [`Sink::place`].
    fn placed(&mut self, len: u64) -> io::Result<()> {
        let _ = len;
        Ok(())
    }
}

/// A file written at offsets ([`Sink::place`]).
pub struct Placing(std::fs::File);

impl Placing {
    fn read_at(&self, at: u64, d: &mut [u8]) -> io::Result<()> {
        #[cfg(unix)]
        return std::os::unix::fs::FileExt::read_exact_at(&self.0, d, at);
        #[cfg(windows)]
        {
            let (mut at, mut d) = (at, d);
            while !d.is_empty() {
                match std::os::windows::fs::FileExt::seek_read(&self.0, d, at)? {
                    0 => return Err(io::ErrorKind::UnexpectedEof.into()),
                    n => (at, d) = (at + n as u64, &mut d[n..]),
                }
            }
            Ok(())
        }
    }
}

impl aqr::sink::Place for Placing {
    fn write_at(&self, at: u64, d: &[u8]) -> io::Result<()> {
        #[cfg(unix)]
        return std::os::unix::fs::FileExt::write_all_at(&self.0, d, at);
        #[cfg(windows)]
        {
            let (mut at, mut d) = (at, d);
            while !d.is_empty() {
                match std::os::windows::fs::FileExt::seek_write(&self.0, d, at)? {
                    0 => return Err(io::ErrorKind::WriteZero.into()),
                    n => (at, d) = (at + n as u64, &d[n..]),
                }
            }
            Ok(())
        }
    }
}

impl Sink for Vec<u8> {
    fn written(&mut self) -> io::Result<Box<dyn Deref<Target = [u8]> + '_>> {
        Ok(Box::new(&self[..]))
    }

    fn restart(&mut self) -> io::Result<()> {
        self.clear();
        Ok(())
    }
}

/// A file written from its start, opened to be read as well (it is mapped to be read back).
impl Sink for std::fs::File {
    fn written(&mut self) -> io::Result<Box<dyn Deref<Target = [u8]> + '_>> {
        // SAFETY: the file is the archive being written, changed by nothing else while it is
        // read back (the map borrowing the file)
        Ok(Box::new(unsafe { memmap2::Mmap::map(&*self)? }))
    }

    fn restart(&mut self) -> io::Result<()> {
        self.set_len(0)?;
        self.seek(io::SeekFrom::Start(0)).map(|_| ())
    }

    fn mapped(&self) -> bool {
        true
    }

    fn place(&mut self) -> Option<Arc<Placing>> {
        let empty = self.stream_position().ok()? == 0 && self.metadata().ok()?.len() == 0;
        empty.then(|| self.try_clone().ok().map(|f| Arc::new(Placing(f)))).flatten()
    }

    fn placed(&mut self, len: u64) -> io::Result<()> {
        // (the position shared with the clone written at offsets, on Windows)
        self.seek(io::SeekFrom::Start(len)).map(|_| ())
    }
}

/// An archive given mirror blocks as it is written to a sink.
impl<S: Sink + ?Sized> Sink for aqr::Mirroring<&mut S> {
    fn written(&mut self) -> io::Result<Box<dyn Deref<Target = [u8]> + '_>> {
        self.get_mut().written()
    }

    fn restart(&mut self) -> io::Result<()> {
        aqr::Mirroring::restart(self);
        self.get_mut().restart()
    }

    fn mapped(&self) -> bool {
        self.get_ref().mapped()
    }
}

/// An encrypted archive being written to a file, read back decrypted (into memory not backed
/// by a file, so that nothing decrypted is written out).
impl<F: Write + io::Seek + std::borrow::Borrow<std::fs::File>> Sink for aqr::envelope::Sealer<F> {
    fn written(&mut self) -> io::Result<Box<dyn Deref<Target = [u8]> + '_>> {
        self.finish()?;
        // SAFETY: as of a file above
        let c = unsafe { memmap2::Mmap::map(std::borrow::Borrow::<std::fs::File>::borrow(self.get_ref()))? };
        let u = self.unseal(&c).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
        let mut d = memmap2::MmapMut::map_anon(u.len().max(1))?;
        u.decrypt_into(&mut d[..u.len()], aqr_common::pool::size())
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
        Ok(Box::new(d.make_read_only()?))
    }

    fn restart(&mut self) -> io::Result<()> {
        aqr::envelope::Sealer::restart(self, |f| std::borrow::Borrow::<std::fs::File>::borrow(f).set_len(0))
    }
}

/// [`compress`] of a file of `kind`, with the formula of the archive where it is to be `kept`:
/// else it is dropped before the archive is verified. Where there are `notes`, it is told of as
/// a batch of content 0.
fn compress_with(
    d: &[u8],
    kind: Kind,
    opt: &Options,
    kept: bool,
    notes: Option<progress::Notes>,
    out: &mut dyn Sink,
) -> io::Result<(usize, Option<Formula>)> {
    use progress::Note;
    let opt = &verifying(opt);
    let note = |n: Note| {
        if let Some(notes) = &notes {
            notes.send(n)
        }
    };
    note(Note::Batch { batch: 0, bytes: d.len() as u64, verify: opt.verify, contents: vec![0] });
    compressing(opt);
    let _spill = spilling(opt);
    note(Note::Started(0));
    let watch = notes.as_ref().map(|notes| progress::Files::of(notes, [(0, d)]));
    let mut cx = Cx::new(d, opt);
    cx.pre = opt.verify.then(|| Arc::new(verify::Prevalidator::spawn(opt.threads, &[d])));
    // (the contents of an image or the like sealed into a sink as they are made; of a large file,
    // the sink's blocks written to `out` as they are, after room for the formula block)
    let direct = direct_room(d, kind, opt, &cx.enc).and_then(|room| Some((out.place()?, room)));
    let sink = match &direct {
        Some((place, room)) => {
            let at = (aqr::HEADER_MAGIC.len() as u64) + room;
            aqr::sink::Sink::direct(&cx.enc, archive::SLICE_MIN, place.clone(), at, opt.threads.max(2))
        }
        None => aqr::sink::Sink::new(&cx.enc, archive::SLICE_MIN),
    };
    let order = Arc::new(seal::Order::new(sink));
    *cx.sealing.get_mut() = Some(seal::Sealing { order: order.clone(), slot: seal::ROOT });
    let _item = aqr_common::trace::item(Some(0));
    let analyzing = aqr_common::trace::span("analyze");
    let mut f = recompress_at(d, opt, 0, kind, &cx);
    // (what the file's parts hold of each other, as a batch's files: the same body in two ZIP
    // archives in a tar archive, say)
    f.share_slices(archive::SLICE_MIN);
    // (the root is one output: its parts flattened too)
    f.root.flatten();
    drop(watch);
    drop(analyzing);
    note(Note::Ended(0));
    let memo = cx.pre.take().map(verify::Prevalidator::finish_shared).unwrap_or_default();
    // (the root's slot taken, or never used)
    drop(cx.sealing.take());
    let sink = Arc::into_inner(order).expect("the order held only here").into_sink();
    let r = finish(f, d, opt, &cx, memo, Some((sink, direct)), kept, notes.as_ref(), out)?;
    note(Note::Written(0));
    Ok(r)
}

/// Where an archive is written directly ([`direct_room`]): through what, and the room before the
/// blocks.
type Direct = (Arc<Placing>, u64);

/// Bytes of a file of tensors (aqr-tensors') from which on it is archived directly ([`direct_room`]).
const DIRECT: usize = 1 << 30;

/// [`DIRECT`], or what the tests set.
fn direct_min() -> usize {
    #[cfg(test)]
    if let Some(n) = tests::DIRECT_MIN.get() {
        return n;
    }
    DIRECT
}

/// Where file `d` of `kind` (of [`direct_min`] bytes or more) is archived alone by the formula
/// of a recompressor telling how large its formula comes to at most (aqr-tensors'), each part of
/// it sealed as it is made: the room for its module and formula blocks to leave before the
/// blocks of the sink, which are then written to the archive as they are made rather than held
/// (`docs/streaming-encoder-design.md`, "출력에 바로 쓰기"); what the formula leaves of the room is
/// a spacer block after it. Not where the archive is mirrored (the mirror blocks' places are
/// not planned with the room).
fn direct_room(d: &[u8], kind: Kind, opt: &Options, enc: &aqr::Encoder) -> Option<u64> {
    let tensors = [Format::Gguf, Format::Pth, Format::Safetensors];
    let taken = detect(d)
        .filter(|f| tensors.contains(f))
        .is_some_and(|f| (kind == Kind::Unknown || kind == Kind::Format(f.name())) && format_on(f, opt));
    if d.len() < direct_min() || opt.mirrors || !opt.recompresses() || !taken {
        return None;
    }
    let bound = aqr_tensors::formula::formula_bound(d)?;
    // (the data compressed at most: Zstandard's raw blocks, its frame and the block's header)
    let formula = bound + bound / 128 + 64;
    let modules = enc.module_len(aqr_tensors::formula::MODULE).ok()?;
    let room = formula + modules + 2;
    #[cfg(test)]
    if let Some(n) = tests::DIRECT_ROOM.get() {
        return Some(n);
    }
    Some((room + room % 2) as u64)
}

/// The sealing that the parts of a formula take as they are made, where its sink writes its
/// blocks as they are ([`direct_room`]): the formula of the file itself, made as `sealing` is
/// given (taken by [`recompress_sealed`]).
fn sealing_parts(sealing: &Option<seal::Sealing>) -> Option<Arc<seal::Order>> {
    sealing.as_ref().filter(|s| s.slot == seal::ROOT && s.order.sink().is_direct()).map(|s| s.order.clone())
}

/// Bytes of aqr-lzma's match finders' chains at once while compressing: two of a 64 MiB
/// dictionary, the parts of a stream or the folders of a 7z archive side by side (decoding
/// keeps to one, within its budget of memory).
const LZMA_CHAINS: usize = 2 << 30;

/// Lets aqr-lzma's match finders take [`LZMA_CHAINS`], as compressing starts, and has them keep
/// the plans of its applications to the application bound of `opt` (by the reach they look back,
/// which a stream's record tells).
pub(crate) fn compressing(opt: &Options) {
    aqr_lzma::mf::allow_chains(LZMA_CHAINS);
    let bound = |m| memory::bound_of(opt.application_bound, m);
    aqr_lzma::plan::set_bound(bound(aqr_lzma::formula::MODULE).min(bound(aqr_lzma::formula::LZMA2)));
}

/// The spill of [`Options::spill`] as the store of large buffers, while the guard is kept (and
/// the heap where it cannot be made).
pub(crate) fn spilling(opt: &Options) -> Option<aqr_formula::store::Guard> {
    let spill = spill::Spill::create(opt.spill.as_ref()?).ok()?;
    Some(aqr_formula::store::set(Some(Arc::new(spill))))
}

/// The archive of formula `f` of `d`, written to `out`, of the bytes written; with
/// [`Options::verify`], read back, decoded and compared with `d` (the outputs of applications
/// in `memo` taken from there), and where it is not `d` (a module's bug), the archive of `d` raw
/// written instead. The formula is returned where `kept` (of no bytes of the streams sunk);
/// what is no longer needed (the formula, the streams compressed) is dropped before the
/// decoding. The streams of `sink`, where `f`'s sunk streams are, are written after its own;
/// where it writes its blocks to `out` as they are made (through the place given, after the
/// room given: [`direct_room`]), `f` is sealed into it too, and its module and formula blocks put
/// into the room ([`written_direct`]). The `notes` are told of it encoded and verified. Fails
/// where `out` does.
#[allow(clippy::too_many_arguments)]
fn finish(
    mut f: Formula,
    d: &[u8],
    opt: &Options,
    cx: &Cx,
    memo: verify::Memo,
    sink: Option<(aqr::sink::Sink, Option<Direct>)>,
    kept: bool,
    notes: Option<&progress::Notes>,
    out: &mut dyn Sink,
) -> io::Result<(usize, Option<Formula>)> {
    let (sink, direct) = sink.map_or((None, None), |(s, d)| (Some(s), d));
    use progress::Note;
    let mut mirroring = None;
    let out: &mut dyn Sink = match opt.mirrors {
        true => mirroring.insert(aqr::Mirroring::new(out, opt.size_hint)),
        false => out,
    };
    memory::declare(&mut f, &Native { threads: opt.threads }, &memo.1);
    memory::note_kept(&f, opt.application_bound, d.len() as u64);
    // the streams of a class together, as they compress smaller: as pieces of the streams, not
    // copied, their blocks compressed as they are written; then the sink's
    // (of a file mapped, let go of as it is read: of all of it, again)
    let sum = Some(aqr::checksum_through(d));
    let encoding = aqr_common::trace::span("encode");
    let n = match (sink, direct) {
        (Some(sink), Some((place, room))) => written_direct(&mut f, sink, &place, room, sum, &cx.enc, out)?,
        (sink, _) => {
            let sunk = sink.map(aqr::sink::Sink::close).transpose().map_err(io::Error::other)?.unwrap_or_default();
            let (mut merged, groups) = f.merged();
            merged.streams.extend(sunk_streams(&sunk));
            let pieces: Vec<Vec<&[u8]>> = groups
                .iter()
                .map(|g| g.iter().map(|&(i, a, b)| &f.streams[i].data[a as usize..b as usize]).collect())
                .collect();
            written(out, |w| cx.enc.encode_sunk_to(&merged, &pieces, &sunk, sum, w))?
        }
    };
    // (the input read through: a file mapped, its pages read again as verifying comes to them)
    aqr_formula::store::let_go(d);
    drop(encoding);
    notes.inspect(|n| n.send(Note::Encoded(0, 1, 1)));
    cx.enc.clear();
    let f = kept.then_some(f);
    // (read back from its file, its blocks let go of as they are decoded: not all of the archive
    // resident by the end; not where it is decrypted into memory of its own)
    let mapped = out.mapped();
    let memo = std::cell::RefCell::new(memo);
    let decodes = |c: &[u8]| {
        let _s = aqr_common::trace::span("verify");
        let files = |_: usize, range: Range<usize>| d.get(range).map(<[u8]>::to_vec);
        let m = verify::Cached::new(Native { threads: opt.threads }, std::mem::take(&mut *memo.borrow_mut()), &files);
        // (compared as it is written out, not held whole: a file of parts streams, see
        // `aqr::Archive::write_file`)
        let runner = aqr_decompress::natives(&m);
        // (the bytes compared let go of as it goes, [`LET_GO`] at a time, where `d` is a file mapped)
        let (mut at, mut gone) = (0, 0);
        let decodes = aqr::open(c, &runner).is_ok_and(|mut a| {
            if mapped {
                // SAFETY: the archive's file mapped read only ([`Sink::mapped`])
                unsafe { a.let_go_read() };
            }
            let written = a.write_file(opt.threads, &mut |p: &[u8]| {
                let same = d.get(at..at + p.len()) == Some(p);
                at += p.len();
                if same && at - gone >= LET_GO {
                    aqr_formula::store::let_go(&d[gone..at]);
                    gone = at;
                }
                if same { Ok(()) } else { Err(aqr::Failure::Invalid("not the file".into())) }
            });
            written.is_ok_and(|n| n == d.len() as u64)
        });
        aqr_formula::store::let_go(d);
        notes.inspect(|n| n.send(Note::Verified(0, d.len() as u64)));
        decodes
    };
    // by the hashes recorded, without running a module; else decoded
    let verified = |c: &[u8]| match hashed(c, d, &memo, opt, mapped) {
        Some(same) => same,
        None => decodes(c),
    };
    let r = match n {
        Ok(n) if !opt.verify || verified(&out.written()?) => (n, f),
        // a leaf of other bytes than `d` is not encoded with its checksum
        _ => {
            out.restart()?;
            // (of the bytes of `d`, not a copy of them, but for the formula kept)
            let mut raw = Formula::raw("raw", Vec::new());
            raw.root = Node::slice(0, 0, d.len() as u64);
            let n = written(out, |w| cx.enc.encode_pieces_to(&raw, &[vec![d]], sum, w))?;
            (n.expect("a stream of the whole"), kept.then(|| Formula::raw("raw", d.to_vec())))
        }
    };
    Ok((r.0 + mirroring.map_or(0, |m| m.added() as usize), r.1))
}

/// The streams of formula of a sink's streams `sunk`, after those of the formula's own: as
/// [`Formula::merged`] has them.
fn sunk_streams(sunk: &[aqr::sink::Sunk]) -> impl Iterator<Item = Stream> + '_ {
    sunk.iter().enumerate().map(|(k, s)| Stream {
        class: s.class.clone(),
        kind: s.kind,
        content: false,
        apart: false,
        data: Bytes::sunk(k, s.len as usize),
    })
}

/// The archive of formula `f` whose blocks `sink` writes to `out` as they are made (through
/// `place`, from the room of `room` bytes after the header on, [`direct_room`]), of the bytes
/// written: `f` sealed into it, the room then its module and formula blocks and a spacer block
/// of the rest of it, and the footer after the blocks. Where they take more than the room (which
/// they are not to), the blocks are moved after them. A formula error (a block of the sink no
/// slice takes, as where a recompression gave up having sealed some of it) is given as such,
/// for the archive of the file as it is to be written instead.
fn written_direct(
    f: &mut Formula,
    sink: aqr::sink::Sink,
    place: &Placing,
    room: u64,
    sum: Option<u64>,
    enc: &aqr::Encoder,
    out: &mut dyn Sink,
) -> io::Result<aqr_formula::Result<usize>> {
    use aqr::sink::Place;
    let start = aqr::HEADER_MAGIC.len() as u64;
    #[cfg(test)]
    tests::DIRECTED.with(|n| n.set(n.get() + 1));
    let sealed = sink.seal(f);
    let (sunk, end) = sink.close_direct().map_err(io::Error::other)?;
    let head = sealed.and_then(|()| {
        let (mut merged, groups) = f.merged();
        if !groups.is_empty() {
            return Err(aqr_formula::Error::new("a stream of the formula not sealed"));
        }
        merged.streams.extend(sunk_streams(&sunk));
        enc.direct_head(&merged, &sunk, sum)
    });
    let head = match head {
        Ok(h) => h,
        Err(e) => return Ok(Err(e)),
    };
    // (a spacer of 2 bytes at least after the formula block: the room only shrinks)
    let need = head.len() as u64 + 2;
    let (blocks_at, end) = match need <= room {
        true => (start + room, end),
        false => {
            let by = need - room;
            moved_back(place, start + room, end, by)?;
            (start + need, end + by)
        }
    };
    let spacer = blocks_at - start - head.len() as u64;
    place.write_at(0, &aqr::HEADER_MAGIC)?;
    place.write_at(start, &head)?;
    place.write_at(start + head.len() as u64, &aqr::kill(spacer as usize))?;
    let footer = aqr::footer_bytes((end - start) as usize);
    place.write_at(end, &footer)?;
    let n = end + footer.len() as u64;
    out.placed(n)?;
    Ok(Ok(n as usize))
}

/// Moves bytes `from..end` written through `place` back by `by` bytes, from the end on.
fn moved_back(place: &Placing, from: u64, end: u64, by: u64) -> io::Result<()> {
    use aqr::sink::Place;
    let mut buf = vec![0u8; 8 << 20];
    let mut at = end;
    while at > from {
        let n = (at - from).min(buf.len() as u64);
        at -= n;
        place.read_at(at, &mut buf[..n as usize])?;
        place.write_at(at + by, &buf[..n as usize])?;
    }
    Ok(())
}

/// Whether archive `c` of file `d` gives it back by its content hashes, worked out without
/// running a module by what `memo` recorded (`aqr::Archive::hashed`): its outputs' hashes those
/// of the parts of `d` they are, which their checksums are of. None where any application is not
/// recorded.
fn hashed(c: &[u8], d: &[u8], memo: &std::cell::RefCell<verify::Memo>, opt: &Options, mapped: bool) -> Option<bool> {
    use aqr_formula::chash::Ch;
    let _s = aqr_common::trace::span("verify");
    let native = Native { threads: opt.threads };
    let runner = aqr_decompress::natives(&native);
    let Ok(mut a) = aqr::open(c, &runner) else { return Some(false) };
    if mapped {
        // SAFETY: the archive's file mapped read only ([`Sink::mapped`])
        unsafe { a.let_go_read() };
    }
    let roots = match a.hashed(&*memo.borrow(), &aqr_decompress::identify, opt.threads) {
        Ok(Some(roots)) => roots,
        Ok(None) => return None,
        Err(_) => return Some(false),
    };
    // (the hash of `d` made of those of its pieces, each read once for both, [`LET_GO`] at a
    // time, its pages then let go of: not all of a file mapped resident by the end)
    let (mut at, mut whole, mut of_d) = (0usize, Ch::EMPTY, Ch::EMPTY);
    for (h, sums) in roots {
        let Some(part) = d.get(at..at.saturating_add(h.len() as usize)) else { return Some(false) };
        let mut sum = xxhash_rust::xxh64::Xxh64::new(0);
        for piece in part.chunks(LET_GO) {
            sum.update(piece);
            of_d = of_d.cat(Ch::of(piece));
            aqr_formula::store::let_go(piece);
        }
        // (the checksums of the archive's, [`aqr::checksum`])
        if sums.iter().any(|&s| s != sum.digest()) {
            return Some(false);
        }
        (at, whole) = (at + part.len(), whole.cat(h));
    }
    let same = at == d.len() && whole == of_d;
    aqr_formula::store::let_go(d);
    Some(same)
}

/// Bytes of the input compared in verifying ([`finish`]) between the pages let go of: few calls,
/// and few pages more resident.
const LET_GO: usize = 4 << 20;

/// What `write` writes to `out` (through a buffer), flushed.
fn written<T>(out: &mut dyn Sink, write: impl FnOnce(&mut dyn Write) -> io::Result<T>) -> io::Result<T> {
    let mut w = io::BufWriter::with_capacity(1 << 20, out);
    let t = write(&mut w)?;
    w.flush()?;
    Ok(t)
}

/// [`recompress`] for data `depth` formats deep, within `cx`.
/// A file of `kind` is recompressed as [`Options::file`] and the kind tell ([`Nest::of`]), as a
/// file in an archive of many is, but for a format: that is looked for as in a file of no kind,
/// as a scan finds the file the data starts with, and what is in it where that is turned down
/// (which [`Nest::As`] does only by [`regions`]). Data kept raw is of the kind, or of
/// [`Kind::Text`] where it is text (as [`made`] tells), so archived as text.
pub(crate) fn recompress_at(d: &[u8], opt: &Options, depth: u32, kind: Kind, cx: &Cx) -> Formula {
    let how = match kind {
        Kind::Format(_) => Some(opt.file),
        kind => opt.file.of(kind),
    };
    how.and_then(|how| made(d, opt, depth, how, cx)).map_or_else(
        || {
            let text = how == Some(Nest::As(Format::Text))
                || matches!(kind, Kind::Unknown | Kind::Format(_)) && is_text(detect(d));
            let mut f = Formula::default();
            f.root = f.push_as("raw", if text { Kind::Text } else { kind }, d.to_vec());
            f
        },
        |m| m.cut(d),
    )
}

/// [`recompress_at`], unless `d` is kept raw; the data that a scan keeps between files is not
/// cut out of `d` yet.
pub(crate) fn made(d: &[u8], opt: &Options, depth: u32, how: Nest, cx: &Cx) -> Option<Made> {
    let m = made_alone(d, opt, depth, how, cx);
    // (where `d` is part of a file, that part is done)
    aqr_common::progress::done(d);
    m
}

fn made_alone(d: &[u8], opt: &Options, depth: u32, how: Nest, cx: &Cx) -> Option<Made> {
    let text = how == Nest::As(Format::Text) || how == Nest::Scan && is_text(detect(d));
    // (the data of a DEFLATE stream archived along with it, kept as it is: made of it)
    if depth < opt.depth
        && let Some(f) = cx.bodies.as_ref().and_then(|b| b.formula(d))
    {
        return Some(Made::from(f));
    }
    match how {
        _ if depth >= opt.depth || !opt.recompresses() => None,
        Nest::Exact => detect(d).and_then(|format| recompress_as(format, d, opt, depth, cx)).map(Made::from),
        // JSON lines or a table where aqr-jsonl or aqr-xsv takes it (and it is [`smaller`]);
        // else text as aqr-wrt tells of it alone (not [`smaller`]), so that the same text is
        // taken alike wherever it is; else scanned as other data is, or kept raw where named as
        // text
        _ if text => match table(d, opt, depth, cx).or_else(|| recompress_as(Format::Text, d, opt, depth, cx)) {
            Some(f) => Some(Made::from(f)),
            None if how == Nest::Scan => {
                scan(d, None, opt, depth, cx).filter(|m| smaller(&m.f, m.gaps.as_deref(), d, opt, cx))
            }
            None => None,
        },
        // a data file of an unknown archive format whose directory was found (its members
        // taken apart before a scan takes one of them for the file); else its samples after a
        // header telling their size, or its tables of records, if any
        Nest::Scan if pak::has(d, cx) => recompress_as(Format::Pak, d, opt, depth, cx).map(Made::from),
        Nest::Scan => scan(d, None, opt, depth, cx)
            .filter(|m| smaller(&m.f, m.gaps.as_deref(), d, opt, cx))
            .or_else(|| recompress_as(Format::Sized, d, opt, depth, cx).map(Made::from))
            .or_else(|| records(d, opt, depth, cx)),
        Nest::As(format) => {
            scan(d, Some(format), opt, depth, cx).filter(|m| smaller(&m.f, m.gaps.as_deref(), d, opt, cx))
        }
    }
}

/// `d`, data in which a scan finds no file, as its tables of records and chains of entries
/// where aqr-infer-recs takes them (a quick estimate telling they may save 5% at least) and the
/// formula is [`smaller`].
fn records(d: &[u8], opt: &Options, depth: u32, cx: &Cx) -> Option<Made> {
    recompress_as(Format::Records, d, opt, depth, cx).map(Made::from)
}

/// Whether data detected as `format` is text: taken as a table, else as text ([`made`]).
fn is_text(format: Option<Format>) -> bool {
    format.is_some_and(|f| f.spec().text)
}

/// `d`, text, as JSON lines if aqr-jsonl takes it, else as a table if aqr-xsv does (and the
/// formula is [`smaller`]).
fn table(d: &[u8], opt: &Options, depth: u32, cx: &Cx) -> Option<Formula> {
    let jsonl = aqr_jsonl::formula::starts(d).then(|| recompress_as(Format::Jsonl, d, opt, depth, cx)).flatten();
    jsonl.or_else(|| aqr_xsv::formula::starts(d).then(|| recompress_as(Format::Xsv, d, opt, depth, cx)).flatten())
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
            // (of the store where it is large, see [`Options::spill`])
            let mut data = aqr_formula::store::Writer::new(Some(gaps.iter().map(Range::len).sum()));
            for g in gaps {
                data.write(&d[g]);
            }
            f.streams[0].data = data.into_bytes();
        }
        f
    }

    /// [`Made::cut`] out of `d` itself, in place where it is its own (a copy of what is kept,
    /// where it is shared: a file mapped, not to be copied whole).
    fn cut_from(self, d: Bytes) -> Formula {
        if d.is_shared() {
            return self.cut(&d);
        }
        let Made { mut f, gaps } = self;
        let mut d = d.into_vec();
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
            f.streams[0].data = d.into();
        }
        f
    }
}

/// A stream of LZFSE and LZVN blocks, at level 9 only, if its data (as its blocks' headers
/// tell) fits in what is left of the budget ([`Options::expansion`]). The data is many times
/// the streams (14 times in the asset catalogs of /Applications: 3.7 GB of 272.5 MB), and
/// recompressing them takes 13% off at some 8 MB/s (34 s at level 8 rather than 1.3 s, peak
/// 8.2 GB; [`ahead`]). The LZVN streams there (small data) gain nothing at the default level
/// and are left out too ([`format_on`]).
fn bvx(d: &[u8], opt: &Options, cx: &Cx) -> Option<Formula> {
    let (_, n) = aqr_bvx::bvx::extent(d)?;
    if !cx.inflate(n) {
        return None;
    }
    aqr_bvx::formula::encode(d, opt.threads)
}

/// aqr-wim's formula of `d`: of its data made again where needed, rather than held, where its
/// bytes are shared ([`aqr_formula::store::find`]: of the input mapped, or of the spill), so
/// that [`make_later`] makes the data of each content as it comes to it.
fn wim(d: &[u8], threads: usize) -> Option<Formula> {
    match aqr_formula::store::find(d) {
        Some(b) => aqr_wim::formula::encode_shared(&b, threads),
        None => aqr_wim::formula::encode(d, threads),
    }
}

/// Whether LZX streams (CAB's folders, CHM's sections) are taken at the level of `opt`: from
/// level 9, as they split at about 22 MB/s of data on one thread a stream, and a CAB or CHM file
/// alone gains little for it (441 CAB files: -1.1% for 73.7 -> 290 s; 139 CHM files: -18.3% for
/// 3.2 -> 14.8 s). What pays is the files archived along with them (40 LZX CAB files with the
/// files they hold: -33%).
/// (CHM files are not known by their names: one known so and turned down would not be scanned.)
fn lzx_on(opt: &Options) -> bool {
    opt.level.scale() >= LZX_LEVEL
}

/// The level LZX streams are taken from ([`lzx_on`]).
const LZX_LEVEL: u8 = 9;

/// Whether RAR's streams with PPMd blocks are taken apart (and PPMd rebuilt): from level 9 only,
/// as rebuilding PPMd is slow and its data seldom archives smaller apart (high-order PPMd is
/// strong); below, they are turned down at their first PPMd block.
fn ppmd_on(opt: &Options) -> bool {
    opt.level.scale() >= 9
}

/// Whether files of `format` are taken at the level of `opt` (else kept as they are): none at
/// levels 0 and 1, which recompress nothing ([`Options::recompresses`]); LZMA and
/// xz streams (aqr-lzma), 7z archives (aqr-7z, and aqr-lzma on their folders) and GIF files
/// (aqr-gif) from level 4, as they are slow for their bytes; streams of LZFSE and LZVN blocks
/// ([`bvx`]) and CHM files ([`lzx_on`]) from level 9. Parts of a format taken (CAB's LZX
/// folders, RAR's PPMd blocks) are its recompressor's to turn down: [`lzx_on`], [`ppmd_on`].
pub(crate) fn format_on(format: Format, opt: &Options) -> bool {
    opt.recompresses() && opt.level.scale() >= format.spec().level
}

/// `d` as a file of `format`, with its contents nested, if its recompressor takes it (and the
/// formula is [`smaller`]).
fn recompress_as(format: Format, d: &[u8], opt: &Options, depth: u32, cx: &Cx) -> Option<Formula> {
    recompress_with(format, d, Kind::Unknown, opt, depth, cx)
}

/// [`recompress_as`], the data of a zlib stream or gzip member (without a name) of kind
/// `contents`.
fn recompress_with(format: Format, d: &[u8], contents: Kind, opt: &Options, depth: u32, cx: &Cx) -> Option<Formula> {
    // (the sealing kept from what this recompressor makes, given back where it is not used)
    let mut sealing = cx.sealing.take();
    let f = recompress_sealed(format, d, contents, opt, depth, cx, &mut sealing);
    if sealing.is_some() {
        *cx.sealing.borrow_mut() = sealing;
    }
    f
}

/// [`recompress_with`], the contents of the formula sealed by `sealing` (taken) where it makes
/// its files apart, and is not compared with `d` ([`smaller`]).
#[allow(clippy::too_many_arguments)]
fn recompress_sealed(
    format: Format,
    d: &[u8],
    contents: Kind,
    opt: &Options,
    depth: u32,
    cx: &Cx,
    sealing: &mut Option<seal::Sealing>,
) -> Option<Formula> {
    if !format_on(format, opt) || !fits(format, d.len()) {
        return None;
    }
    let _s = aqr_common::trace::span(format.name());
    #[cfg(test)]
    tests::TRIED.with(|t| t.borrow_mut().push(format));
    let ready = cx.ready.borrow_mut().remove(&(d.as_ptr() as usize, d.len(), format));
    let prepared = ready.is_some();
    let words = cx.words.borrow().clone();
    let (words, held) = (words.as_deref(), cx.held.as_deref());
    let f = match ready {
        Some(f) => f,
        None => match recompressor(format) {
            Recompressor::Cx(r) => {
                catch_unwind(AssertUnwindSafe(|| attempted(format, d, || r(d, contents, opt, depth, cx))))
                    .ok()
                    .flatten()
            }
            _ => {
                // (of the file itself, archived directly: its parts sealed as they are made, each
                // evaluated beforehand on its own)
                let order = sealing_parts(sealing);
                let (seal, pre) = (order.as_deref(), cx.pre.as_deref());
                pure(format, d, &Pure { opt, depth, words, once: &cx.once, held, seal, pre }).flatten()
            }
        },
    }?;
    // (parts sealed already, evaluated beforehand as they were)
    let sealed = f.streams.iter().any(|s| s.data.sunk_in().is_some());
    let mut f = f;
    f.mark_contents();
    // (an application over the bound: this recompression is not used, as if the recompressor did
    // not take the data; a formula of data not made yet is not looked at)
    if !f.streams.iter().any(|s| s.data.is_lazy())
        && let Some((module, needed)) = memory::exceeding(&f, &Native { threads: 1 }, opt.application_bound)
    {
        memory::gave_up(module, needed, d.len() as u64);
        return None;
    }
    aqr_formula::store::spill_streams(&mut f);
    if format == Format::Text {
        cx.note_coded(&f, d);
    }
    // evaluated beforehand while its contents are recompressed (once it is known not to be
    // taken into a set, where made beforehand as a PNG file among contents: it is put off, as
    // only PNG files are taken into sets), a copy of it sharing its bytes: the contents may take
    // the bytes of their streams out of it as they are made ([`make_later`]), let go of once
    // the prevalidator is done with them too
    f.streams.iter_mut().for_each(|s| s.data.share());
    let shared = Arc::new(f.clone());
    // (a formula of data not made yet, aqr-wim's: its applications evaluated with their contents
    // as they are made, [`make_later`], else once its data is made, below)
    let lazy = f.streams.iter().any(|s| s.data.is_lazy());
    if let Some(pre) = cx.pre.as_ref().filter(|_| !lazy && !sealed) {
        match prepared && format == Format::Png {
            true => pre
                .job(shared.clone(), d)
                .into_iter()
                .for_each(|j| cx.deferred.borrow_mut().push((d.as_ptr() as usize, j))),
            false if format == Format::Text && !cx.once.evaluated(d) => {}
            false => pre.submit(shared.clone(), d),
        }
    }
    let mark = cx.deferred.borrow().len();
    // (an ar, tar or cpio archive only bundles files: they are as deep as it is; and so are
    // those an NSIS installer stores)
    let deeper;
    let opt = match format.is_bundle()
        || format == Format::Pe && (nsis::in_pe(d).is_some_and(|b| !b.is_empty()) || aqr_inno::end(d).is_some())
    {
        true => {
            deeper = Options { depth: opt.depth.saturating_add(1), ..opt.clone() };
            &deeper
        }
        false => opt,
    };
    drop(shared);
    let watches = told_as_second_half(&f, d);
    let words = (format == Format::Jsonl || format.is_bundle()).then(|| segment_words(&f, opt, cx)).flatten();
    // (where the data put off is, and what the formula is, as the contents may take its streams
    // out of it)
    let bases: Vec<usize> = f.streams.iter().map(|s| addr(&s.data)).collect();
    let of = bundle::Kinds::of(&f);
    cx.parts_checked.set(false);
    if of.files_apart {
        *cx.sealing.borrow_mut() = sealing.take();
    }
    let children = nested(&mut f, d, opt, depth, cx);
    if words.is_some() {
        cx.share_words(None);
    }
    drop(watches);
    // (data not made by the contents made: made now, and the formula evaluated as others are
    // where its contents' applications were not)
    if lazy {
        f.streams.iter_mut().filter(|s| s.data.is_lazy()).for_each(|s| s.data = s.data.made());
        if let Some(pre) = cx.pre.as_ref().filter(|_| !cx.parts_checked.get()) {
            pre.submit(Arc::new(f.clone()), d);
        }
    }
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
        aqr_formula::store::spill_streams(&mut f);
    }
    // the contents' put off, but those taken into a set
    let deferred: Vec<_> = cx.deferred.borrow_mut().drain(mark..).collect();
    if let Some(pre) = &cx.pre {
        deferred.into_iter().filter(|(at, _)| !taken.contains(at)).for_each(|(_, j)| pre.send(j));
    }
    // (text as aqr-wrt tells, see [`made`])
    (format == Format::Text || smaller_as(&f, None, d, of, opt, cx)).then_some(f)
}

/// Where bytes `b` are, as an address (0 for bytes not made yet, or sunk).
fn addr(b: &Bytes) -> usize {
    match b.is_lazy() || b.sunk_in().is_some() {
        true => 0,
        false => b.as_ptr() as usize,
    }
}

/// Text of the contents of a formula shorter than this in all is coded with dictionaries of each
/// text's own (as [`archive`]'s batches have it).
const SEGMENT_WORDS_MIN: usize = 256 << 10;

/// Sets [`Cx::words`] for the contents of `f`, where none is set: a dictionary of the text of its
/// contents (for JSON lines cut into segments, those of [`Kind::Text`]; for an archive that only
/// bundles files, the members that are text as [`made`] tells), at least [`SEGMENT_WORDS_MIN`]
/// bytes in all, so that it is coded with one dictionary, as it is where the file is one segment,
/// and the same text of members is coded alike (the files of a tar archive, as they are on their
/// own in an archive of files). Returns it, to be unset once they are made.
fn segment_words(f: &Formula, opt: &Options, cx: &Cx) -> Option<Arc<aqr_wrt::formula::Shared>> {
    // (of one segment, the file's text has a dictionary of its own anyway)
    let apart = bundle::bundles(f);
    if cx.words.borrow().is_some() || !(apart || matches!(f.root, Node::Concat(_))) {
        return None;
    }
    let texts: Vec<&[u8]> = match apart {
        true => {
            let mut texts = Vec::new();
            member_texts(&f.root, f, &mut texts);
            texts
        }
        false => f
            .streams
            .iter()
            .filter(|s| s.content && s.kind == Kind::Text && aqr_wrt::formula::starts(&s.data))
            .map(|s| &s.data[..])
            .collect(),
    };
    if texts.len() < 2 || texts.iter().map(|t| t.len()).sum::<usize>() < SEGMENT_WORDS_MIN {
        return None;
    }
    let words = Arc::new(aqr_wrt::formula::Shared::of(&texts, opt.threads)?);
    cx.share_words(Some(words.clone()));
    Some(words)
}

/// The contents below node `n` of `f` that are text ([`segment_words`]), made already.
fn member_texts<'a>(n: &Node, f: &'a Formula, out: &mut Vec<&'a [u8]>) {
    match n {
        Node::Concat(ns) | Node::Pack(ns) => ns.iter().for_each(|n| member_texts(n, f, out)),
        &Node::Slice { stream, .. } if is_content(f, stream) => slices_of(n, f, &mut |d| {
            if is_text(detect(d)) && aqr_wrt::formula::starts(d) && !aqr_obj::formula::starts(d) {
                out.push(d);
            }
        }),
        Node::Apply(a) => a.inputs.iter().for_each(|n| member_texts(n, f, out)),
        _ => {}
    }
}

/// The bytes of the slices below node `n` of `f` that are made already.
fn slices_of<'a>(n: &Node, f: &'a Formula, each: &mut dyn FnMut(&'a [u8])) {
    match n {
        &Node::Slice { stream, start, len } => {
            let d = f.streams.get(stream).filter(|s| !s.data.is_lazy()).map(|s| &s.data[..]);
            if let Some(d) = d.and_then(|d| d.get(start as usize..)?.get(..len as usize)) {
                each(d);
            }
        }
        Node::Concat(ns) | Node::Pack(ns) => ns.iter().for_each(|n| slices_of(n, f, each)),
        _ => {}
    }
}

/// Tells of the work on the streams of `f` that are data of their own (not of `d`, the data
/// `f` is of), as their contents are made in turn ([`nested`]), as of the second half of `d`,
/// each stream its share by size: the first is the formula's own, as far as its recompressor
/// tells ([`aqr_common::progress`]).
fn told_as_second_half(f: &Formula, d: &[u8]) -> Vec<aqr_common::progress::Watch> {
    let within = |s: &[u8]| d.as_ptr_range().contains(&s.as_ptr());
    let own: Vec<&[u8]> = f
        .streams
        .iter()
        .filter(|s| !s.data.is_lazy() && s.data.sunk_in().is_none())
        .map(|s| &s.data[..])
        .filter(|s| !s.is_empty() && !within(s))
        .collect();
    let total: usize = own.iter().map(|s| s.len()).sum();
    let (half, rest) = (d.len() / 2, d.len() - d.len() / 2);
    let at = |n: usize| half + (n as u128 * rest as u128 / total.max(1) as u128) as usize;
    let mut n = 0;
    own.iter()
        .map(|s| {
            let part = &d[at(n)..at(n + s.len())];
            n += s.len();
            aqr_common::progress::alias(s, part)
        })
        .collect()
}

/// Unless [`Options::smaller`] is off, or the level is 7 or more (DEFLATE always rebuilt, a few
/// seconds of 10gb), what tells whether recompressing a stream may pay, for its data `depth`
/// formats deep: at the level of the archive, files looked for as [`Nest::of`] tells (scanned,
/// unless nested formats would go deeper than allowed). An ar, tar or cpio archive is scanned
/// for the files in it, as it gains nothing itself; text is told by its data, as aqr-wrt takes
/// no more than a tenth off it.
pub(crate) fn probe(opt: &Options, depth: u32) -> Option<Probe> {
    if opt.level.scale() >= 7 {
        return None;
    }
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
///
/// An ar, tar or cpio archive (alone, or found by a scan) that the files archived together hold
/// is taken whatever its size: its members' formulas are theirs, whose streams are stored once
/// ([`Formula::share_slices`]), though they cost in full here (the streams of a member that is a
/// PE file, say, are not its bytes, which the files are held by). The streams of the formula
/// taken of other data the files hold cost nothing for what holds that data ([`Cx::note_held`]).
/// An ISO 9660 or UDF image is taken whatever its size too: its formula is its own bytes, cut
/// into the files and the rest, which cost no more apart (and comparing would compress all of
/// an image twice, a DVD's 3 GB). So is a WIM file: its LZX chunks are compressed apart, 32 KiB
/// each, and their data compresses better together (by far more than the records cost, where
/// it is not compressed already; where it is, the records are small); comparing took 14 of
/// 24 s of a boot.wim of 169 MB. So is a scan's formula whose files are all of the data, each
/// of them compared already (or text, not compared: [`made`]). So is a PPMd file's, its header
/// cut off ([`takes_place`]), which costs no more apart: a file whose data starts another's
/// is stored once only where both are cut alike. So is a ProRes file's of [`PRORES_UNCOMPARED`]
/// bytes or more: its frames are coded to a fraction of their bytes, which zstd of the file never
/// nears, and the comparison costs 3.7 s on one thread at the end for 1.3 GB. A small one is
/// compared still: what zstd makes of nearly blank frames may beat the formula's modules. So is a
/// file of tensors' (aqr-tensors'): its encoder codes a run only where that comes to less than its
/// bytes (which zstd hardly compresses: weights), and comparing compresses gigabytes twice on one thread (3.6 of
/// 6.3 s for a model of 1.2 GB).
fn smaller(f: &Formula, gaps: Option<&[Range<usize>]>, d: &[u8], opt: &Options, cx: &Cx) -> bool {
    smaller_as(f, gaps, d, bundle::Kinds::of(f), opt, cx)
}

/// The size of a ProRes file from which on [`smaller`] takes its formula without comparing it.
const PRORES_UNCOMPARED: usize = 16 << 20;

/// [`smaller`] of formula `f` of what `of` tells, as its recompressor made it: once its
/// contents are in place, its own streams may be gone (a WIM file's data, all of it cut into
/// its contents' formulas).
fn smaller_as(f: &Formula, gaps: Option<&[Range<usize>]>, d: &[u8], of: bundle::Kinds, opt: &Options, cx: &Cx) -> bool {
    let bundled = of.bundles && cx.held_by_others(d, d);
    if bundled {
        cx.note_bundled(f);
    }
    let covered = gaps.is_some_and(|g| g.iter().all(Range::is_empty));
    // (an image coded against another's code holds a copy of it, which would only be compressed
    // to be taken off: the code filtered, it is smaller)
    let against = f.streams.iter().any(|s| s.class == exe::REFERENCE);
    // (a file keeping DEFLATE streams that files archived along with it are made of)
    let inflated = cx.bodies.as_ref().is_some_and(|b| b.holds_streams(d));
    let smaller = !opt.smaller
        || bundled
        || covered
        || against
        || inflated
        || of.files_apart
        || aqr_ppmd::formula::is(f)
        || (d.len() >= PRORES_UNCOMPARED && aqr_prores::formula::is(f))
        || aqr_tensors::formula::is(f)
        || {
            #[cfg(test)]
            tests::COMPARED.with(|c| c.borrow_mut().push(d.len()));
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
        if !bundled && d.len() >= SHARE_MIN && cx.held_by_others(d, d) {
            cx.note_held(f);
        }
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
    // (a slice of all of it anywhere: at the last byte of a block, over the most blocks)
    let around = description_bound(&Node::slice(0, aqr_formula::aqr::BLOCK as u64 - 1, d.len() as u64));
    let sizes: Vec<_> = found.iter().map(|(_, n, child)| (*n, bound_of(child).saturating_add(around))).collect();
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
    let mut made = Ahead::default();
    // (for a file that is all of `d` only)
    let mut sealing = cx.sealing.take();
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
            if format == Format::Bvx && format_on(format, opt) && !made.0.contains_key(&p) {
                ahead(d, p, &turned_down, opt, depth, cx, &mut made);
            }
            let child = match made.0.remove(&p) {
                Some(a) => a.take(&d[p..p + n], cx),
                None if p == 0 && n == d.len() => {
                    *cx.sealing.borrow_mut() = sealing.take();
                    let f = recompress_as(format, d, opt, depth, cx);
                    sealing = cx.sealing.take();
                    f
                }
                None => recompress_as(format, &d[p..p + n], opt, depth, cx),
            };
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
                None if p == 0 && first.is_some() => {
                    made.give_back(cx);
                    return None;
                }
                None => p = detect::next_start(d, p + 1),
            },
        }
    }
    made.give_back(cx);
    Some(found)
}

/// LZFSE and LZVN streams made ahead of a scan ([`ahead`]), by where they start: each its
/// formula, if taken, what is put off of its prevalidation, and the bytes of its data taken
/// from the budget.
#[derive(Default)]
struct Ahead(HashMap<usize, Early>);

struct Early {
    f: Option<Formula>,
    deferred: Vec<(usize, verify::Deferred)>,
    /// What it noted ([`Cx::coded_held`]).
    notes: HashSet<(usize, u64)>,
    inflated: u64,
    /// What the encoder it was sized with kept of the streams it compressed (without its context).
    enc: aqr::Kept,
}

impl Early {
    /// The formula of stream `d`, as [`recompress_as`] would give it in `cx`.
    fn take(self, d: &[u8], cx: &Cx) -> Option<Formula> {
        cx.deferred.borrow_mut().extend(self.deferred);
        cx.adopt_notes(self.notes);
        // (so that what holds it is sized without compressing its streams again)
        cx.enc.adopt(self.enc);
        if self.f.is_some() {
            cx.enc.forget(d);
        }
        self.f
    }
}

impl Ahead {
    /// Gives the budget of the streams the scan did not come to back to `cx`.
    fn give_back(self, cx: &Cx) {
        let n: u64 = self.0.values().map(|m| m.inflated).sum();
        cx.inflate.set(cx.inflate.get() + n);
    }
}

/// At most this many streams are made ahead at once ([`ahead`]): a number of its own, not the
/// threads', so that the budget they take (and the archive) does not depend on the threads.
const AHEAD: usize = 64;

/// Makes the LZFSE and LZVN streams of `d` from `p` on at once, into `made`, as the scan of
/// [`find`] would come to them one after another if each is taken: up to [`AHEAD`], up to the
/// next file of another format, or to a stream whose data does not fit in the budget left (in
/// the scan's order). Each is made in a context of its own ([`Apart`]), its data taken from
/// `cx`'s budget beforehand, with no more budget for its contents. An asset catalog holds
/// thousands of them, which the scan would make one after another (Windows App's of 72 MB, its
/// data 1.1 GB, at 1.8 threads).
fn ahead(d: &[u8], p: usize, turned_down: &[(Format, usize)], opt: &Options, depth: u32, cx: &Cx, made: &mut Ahead) {
    let mut streams = Vec::new();
    let mut q = p;
    while q < d.len() && streams.len() < AHEAD {
        let format = match q == p {
            true => Some(Format::Bvx),
            false => detect::starts(&d[q..]),
        };
        match format.filter(|f| !turned_down.iter().any(|&(g, end)| g == *f && q < end)) {
            None => {
                q = detect::next_start(d, q + 1);
                continue;
            }
            Some(Format::Bvx) => {}
            Some(_) => break,
        }
        let Some(n) = extent(Format::Bvx, &d[q..]).filter(|&n| n > 0) else { break };
        if !made.0.contains_key(&q) {
            let Some((_, m)) = aqr_bvx::bvx::extent(&d[q..q + n]) else { break };
            if !cx.inflate(m) {
                break;
            }
            streams.push((q, n, m));
        }
        q += n;
    }
    let apart = cx.apart();
    let out = aqr_common::pool::par_map(streams.len(), opt.threads, |i| {
        let (q, n, m) = streams[i];
        let own = apart.cx(n, opt);
        own.inflate.set(m);
        let f = recompress_as(Format::Bvx, &d[q..q + n], opt, depth, &own);
        (f, own.deferred.into_inner(), own.coded_held.into_inner(), own.enc.into_kept())
    });
    for ((q, _, inflated), (f, deferred, notes, enc)) in streams.into_iter().zip(out) {
        made.0.insert(q, Early { f, deferred, notes, inflated, enc });
    }
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
    catch_unwind(AssertUnwindSafe(|| format.spec().regions.and_then(|r| r(d)))).ok().flatten()
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

/// The contents of `f` ([`contents`]), a formula of data `depth`
/// formats deep, each its slice and its formula to put in place of the slice ([`place_all`])
/// where it has any application (as many as fit in [`Options::description`]), and the plan of
/// the sets of them (whose members with a parent have no formula); `None` where none has any
/// and there is no set.
fn nested(f: &mut Formula, of: &[u8], opt: &Options, depth: u32, cx: &Cx) -> Option<(Contents, sets::Plan)> {
    f.mark_contents();
    if depth + 1 >= opt.depth {
        return None;
    }
    // (where [`recompress_with`] lets `f`'s contents be sealed: only by [`make_later`])
    let sealing = cx.sealing.take();
    let (prepared, plan) = prepare(f, opt, depth, cx);
    let mut children = Vec::new();
    // (told once: it walks all of `f`, which an image's thousands of files would each do again)
    let apart = bundle::contents_apart(f);
    for n in shared_nodes(f).iter().chain([&f.root]) {
        contents(n, f, apart, opt, depth, cx, &mut children);
    }
    // (sets read their members' bytes from the formula once made: those of a tree's top too)
    let top = depth == 0 && cx.top_sets.is_some();
    make_later(f, of, &|n| !top && !plan.holds(n), opt, depth, cx, sealing, &mut children);
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
            Some(c) => (slice.len() as usize, bound_of(&c.f).saturating_sub(description_bound(slice))),
            None => (0, 0),
        })
        .collect();
    let keep = fit(bound_of(f), &sizes, opt.description);
    if plan.is_empty() && !children.iter().zip(&keep).any(|((_, child), &keep)| keep && child.is_some()) {
        return None;
    }
    let children = children.into_iter().zip(keep).map(|((slice, child), keep)| (slice, child.filter(|_| keep)));
    Some((children.collect(), plan))
}

/// Makes the contents of [`Cx::later`] (the files of an archive) at once, on up to
/// [`Options::threads`] threads, each in a context of its own (with the formula made of it
/// beforehand, if any, and the same prevalidator), into `children`; what is put off of their
/// prevalidation goes to `cx`'s. A content that is all of its stream, and its only slice, takes
/// the stream's bytes out of `f`, and is cut out of them as soon as it is made: they are let go
/// of then rather than once all are made (the data of a WIM file's resources, a stream each),
/// where nothing else holds them (only those of slices that `take`s). Where it takes no place
/// they are put back. Their formulas are sealed by `sealing` where `f` makes its files apart.
#[allow(clippy::too_many_arguments)]
fn make_later(
    f: &mut Formula,
    of: &[u8],
    take: &dyn Fn(&Node) -> bool,
    opt: &Options,
    depth: u32,
    cx: &Cx,
    sealing: Option<seal::Sealing>,
    children: &mut Contents,
) {
    let later = std::mem::take(&mut *cx.later.borrow_mut());
    if later.is_empty() {
        return;
    }
    // contents of data not made yet (aqr-wim's), made as each is come to, and the application of
    // each then evaluated with it (by its stream: the application, and where its output is in
    // `of`)
    let lazy: Vec<Option<usize>> = later
        .iter()
        .map(|(_, n, _)| match *n {
            Node::Slice { stream, .. } if f.streams.get(stream).is_some_and(|s| s.data.is_lazy()) => Some(stream),
            _ => None,
        })
        .collect();
    let apps = applications_of_lazy(f);
    let mut slices = vec![0usize; f.streams.len()];
    f.visit(&mut |n| {
        if let &Node::Slice { stream, .. } = n
            && let Some(n) = slices.get_mut(stream)
        {
            *n += 1;
        }
    });
    let whole = |f: &Formula, n: &Node| match *n {
        Node::Slice { stream, start: 0, len } if take(n) => {
            (slices[stream] == 1 && len > 0 && len == f.streams[stream].data.len() as u64).then_some(stream)
        }
        _ => None,
    };
    let owned: Vec<Option<usize>> = later.iter().map(|(_, n, _)| whole(f, n)).collect();
    let members: Vec<bool> = later.iter().map(|(_, n, _)| !take(n)).collect();
    // (those not taken made now, as others are)
    for (l, o) in lazy.iter().zip(&owned) {
        if let (Some(s), None) = (l, o) {
            f.streams[*s].data = f.streams[*s].data.made();
        }
    }
    let lazy: Vec<Option<usize>> = lazy.into_iter().zip(&owned).map(|(l, o)| l.filter(|_| o.is_some())).collect();
    let pre = cx.pre.clone().filter(|_| lazy.iter().flatten().any(|s| apps.contains_key(s)));
    cx.parts_checked.set(cx.parts_checked.get() || pre.is_some());
    // the contents' formulas sealed as they are made, in their order, where this formula's
    // contents are: each in its slot, those of a stream taken that take no place as they are;
    // those that would pass the bound of the description not (kept as they are), the bound
    // taken in their order (the sizes as [`nested`] fits them)
    let sealing = sealing.filter(|_| bundle::files_apart(f));
    let slots = sealing.as_ref().map(|s| s.order.slots(s.slot, later.len()));
    let budget = Arc::new(std::sync::Mutex::new(bound_of(f)));
    let sealed: Arc<Vec<std::sync::Mutex<Option<Made>>>> =
        Arc::new((0..later.len()).map(|_| std::sync::Mutex::new(None)).collect());
    let data = |n: &Node| {
        let &Node::Slice { stream, start, len } = n else { unreachable!("a slice") };
        &f.streams[stream].data[start as usize..][..len as usize]
    };
    // (the data that is made already)
    let peek = |n: &Node| match n {
        &Node::Slice { stream, .. } if f.streams[stream].data.is_lazy() => None,
        n => Some(data(n)),
    };
    // the formulas made beforehand, by where their data is
    let mut by_at: HashMap<usize, Vec<_>> = HashMap::new();
    for (k, v) in std::mem::take(&mut *cx.ready.borrow_mut()) {
        by_at.entry(k.0).or_default().push((k, v));
    }
    let ready: Vec<std::sync::Mutex<Vec<_>>> = later
        .iter()
        .map(|(_, n, _)| {
            std::sync::Mutex::new(peek(n).and_then(|d| by_at.remove(&(d.as_ptr() as usize))).unwrap_or_default())
        })
        .collect();
    cx.ready.borrow_mut().extend(by_at.into_values().flatten());
    let images: Vec<&[u8]> = later.iter().filter_map(|(_, n, _)| peek(n)).filter(|d| exe::remappable(d)).collect();
    let remaps = (images.len() > 1).then(|| Arc::new(exe::plan(&images, opt.threads, opt.block / 2)));
    let apart = Apart { remaps, ..cx.apart() };
    let files = cx.files.as_deref();
    // the costliest first, so that a long one is not left to the end, alone (but in their
    // order where sealed: each waits for those before it)
    let mut order: Vec<usize> = (0..later.len()).collect();
    if sealing.is_none() {
        order.sort_by_cached_key(|&i| std::cmp::Reverse(peek(&later[i].1).map_or(later[i].1.len() * 50, cost)));
    }
    // (the bytes of the streams taken, shared so as to be moved without a copy)
    let taken: Vec<std::sync::Mutex<Option<Bytes>>> = owned
        .iter()
        .map(|&s| {
            std::sync::Mutex::new(s.map(|s| {
                let mut b = std::mem::take(&mut f.streams[s].data);
                b.share();
                b
            }))
        })
        .collect();
    let fr = &*f;
    let data = |n: &Node| {
        let &Node::Slice { stream, start, len } = n else { unreachable!("a slice") };
        &fr.streams[stream].data[start as usize..][..len as usize]
    };
    let make = |i: usize| {
        let (_, n, nest) = &later[i];
        let mine = taken[i].lock().unwrap().take().map(|b| b.made());
        let d = mine.as_deref().unwrap_or_else(|| data(n));
        // (the application of data made now evaluated with it)
        if let (Some(pre), Some((app, at))) = (&pre, lazy[i].and_then(|s| apps.get(&s))) {
            let s = lazy[i].expect("a stream");
            let part = fr.part(app, &|x| match x == s {
                true => mine.clone().expect("taken"),
                false => fr.streams[x].data.clone(),
            });
            let out = &of[*at..*at + app.len() as usize];
            pre.submit(Arc::new(part), out);
            // (its output hashed: of a WIM file mapped, its chunks, read through again)
            aqr_formula::store::let_go(out);
        }
        let mut own = apart.cx(d.len(), opt);
        *own.sealing.get_mut() =
            sealing.as_ref().zip(slots.as_ref()).map(|(s, slots)| seal::Sealing { slot: slots[i], ..s.clone() });
        own.ready.borrow_mut().extend(std::mem::take(&mut *ready[i].lock().unwrap()));
        files.inspect(|f| f.tell(d.as_ptr(), false));
        let _item = aqr_common::trace::item(files.and_then(|f| f.content(d.as_ptr())));
        let mut child = made(d, opt, depth + 1, *nest, &own).filter(|c| takes_place(&c.f));
        files.inspect(|f| f.tell(d.as_ptr(), true));
        if let Some((s, slots)) = sealing.as_ref().zip(slots.as_ref()) {
            // (not a member of a set planned, which the sets are made of)
            let member = members[i];
            // (where the data is of a file mapped, an image's file of the input: read through once
            // sealed, but in verifying)
            let input = (d.as_ptr() as usize, d.len());
            let child = child.map(|c| Made::from(c.cut(d)));
            let size = child.as_ref().map(|c| bound_of(&c.f).saturating_sub(description_bound(n)));
            // (the bytes taken let go of now where they have a formula, made again of it in the
            // rare case it is turned down for the bound; but kept where formulas are sealed in
            // it, an image's files, whose streams are no longer at hand to make it)
            let class = owned[i].map(|s| (fr.streams[s].class.clone(), fr.streams[s].kind));
            let sunk = child.as_ref().is_some_and(|c| c.f.streams.iter().any(|s| s.data.sunk_in().is_some()));
            let raw = mine.filter(|_| child.is_none() || sunk);
            let (budget, sealed, threads, limit) = (budget.clone(), sealed.clone(), opt.threads, opt.description);
            s.order.submit(
                slots[i],
                Box::new(move |sink| {
                    let mut made = child.filter(|_| !member);
                    let mut b = budget.lock().unwrap();
                    let mut raw = raw;
                    match size.filter(|_| made.is_some()) {
                        Some(n) if *b + n <= limit => *b += n,
                        // (of the bytes taken only: those of a slice stay in their stream)
                        _ => {
                            if let (None, Some(m), Some(_)) = (&raw, made.take(), &class) {
                                raw = Some(m.f.eval(&Native { threads }).expect("the formula of a content").into());
                            }
                        }
                    }
                    drop(b);
                    // (or the bytes taken as they are, which nothing else holds)
                    let made = made.or_else(|| {
                        raw.zip(class).map(|(d, (class, kind))| {
                            let mut f = Formula::default();
                            f.root = f.push_as(class, kind, d);
                            Made::from(f)
                        })
                    });
                    if let Some(mut m) = made {
                        memory::declare(&mut m.f, &Native { threads }, &memory::Limits::default());
                        sink.seal(&mut m.f).expect("sealed");
                        *sealed[i].lock().unwrap() = Some(m);
                        aqr_formula::store::let_go_at(input.0, input.1);
                    }
                }),
            );
            return (None, None, own.deferred.into_inner(), own.coded_held.into_inner(), None);
        }
        // (cut out of the bytes taken, which are let go of; put back where it takes no place)
        let back = match mine {
            Some(mine) if child.is_some() => {
                child = child.map(|c| Made::from(c.cut(&mine)));
                None
            }
            mine => mine,
        };
        child.iter_mut().for_each(|c| aqr_formula::store::spill_streams(&mut c.f));
        (child, back, own.deferred.into_inner(), own.coded_held.into_inner(), Some(own.enc.into_kept()))
    };
    let made = match sealing.as_ref().zip(slots.as_ref()) {
        // in their order, each let in once the content [`SEAL_AHEAD`] times the threads before it
        // is sealed (the contents made and waiting for their turn held: a long one before them
        // holding it), on workers as they are free (no thread waiting holds one, so that what
        // runs, a long content's own, can take them)
        Some((s, slots)) => aqr_common::pool::dispatch(
            later.len(),
            opt.threads,
            |i| {
                if let Some(back) = i.checked_sub(SEAL_AHEAD * opt.threads.max(1)) {
                    s.order.wait_done(slots[back]);
                }
            },
            |i, ()| make(i),
        ),
        None => aqr_common::pool::par_map(later.len(), opt.threads, |j| make(order[j])),
    };
    if let Some(s) = &sealing {
        s.order.wait(s.slot);
    }
    let mut made: Vec<_> = order.into_iter().zip(made).collect();
    made.sort_unstable_by_key(|&(i, _)| i);
    for (((at, ..), (i, (child, back, deferred, notes, enc))), s) in later.into_iter().zip(made).zip(owned) {
        if let (Some(s), Some(back)) = (s, back) {
            f.streams[s].data = back;
        }
        children[at].1 = child.or_else(|| sealed[i].lock().unwrap().take());
        cx.deferred.borrow_mut().extend(deferred);
        // (in the contents' order, all of them made: none saw another's)
        cx.adopt_notes(notes);
        // (so that what holds it is sized and written without compressing its streams again)
        enc.into_iter().for_each(|e| cx.enc.adopt(e));
    }
}

/// Contents a content made apart starts at most ahead of the first not sealed, where they are
/// sealed ([`make_later`]), for each thread. On the Windows Server DVD (16 threads), the spill's
/// peak and the time: 5.29 GB and 169.1 s with none; 4.45 GB, 171.4 s at 4; 4.24 GB, 175.2 s at
/// 2 (where a long content holds the turn, fewer start meanwhile).
const SEAL_AHEAD: usize = 4;

/// Of each stream of `f` of data not made yet that an application among what its root
/// concatenates takes, that application and where its output starts in `f`'s.
fn applications_of_lazy(f: &Formula) -> HashMap<usize, (Node, usize)> {
    let mut out = HashMap::new();
    let mut at = 0usize;
    let mut walk = vec![&f.root];
    while let Some(n) = walk.pop() {
        match n {
            // (in order)
            Node::Concat(ns) => walk.extend(ns.iter().rev()),
            n => {
                if let Node::Apply(a) = n {
                    let mut lazy = None;
                    a.inputs.iter().for_each(|i| {
                        i.visit(&mut |m| {
                            if let &Node::Slice { stream, .. } = m
                                && f.streams.get(stream).is_some_and(|s| s.data.is_lazy())
                            {
                                lazy.get_or_insert(stream);
                            }
                        })
                    });
                    if let Some(s) = lazy {
                        out.insert(s, (n.clone(), at));
                    }
                }
                at += n.len() as usize;
            }
        }
    }
    out
}

/// About the thread time making data `d` takes, of its bytes and how long its format takes for
/// a byte: thread-milliseconds by MB of the files of each format of 10gb (`--stats`, an M4;
/// rounded), 50 for the others ([`formats::Spec::cost`]).
fn cost(d: &[u8]) -> u64 {
    let per_mb = detect::detect(d).map_or(formats::Spec::DEFAULT.cost, |f| f.spec().cost);
    d.len() as u64 * per_mb
}

/// What a context of data made apart ([`make_later`], [`ahead`]) shares with the context it is
/// made from: the prevalidator, what the files archived together hold, their remaps and words,
/// the text noted and coded once, and whether module blocks are sized.
struct Apart {
    pre: Option<Arc<verify::Prevalidator>>,
    held: Option<Arc<Held>>,
    bodies: Option<Arc<inflated::Bodies>>,
    remaps: Option<Arc<exe::Remaps>>,
    words: Option<Arc<aqr_wrt::formula::Shared>>,
    /// What was noted before it was made ([`Cx::coded_held`]), for each context made of it.
    coded_held: Arc<HashSet<(usize, u64)>>,
    once: Arc<Once>,
    shares_modules: bool,
    paks: Option<Arc<pak::Placed>>,
}

impl Cx {
    fn apart(&self) -> Apart {
        Apart {
            pre: self.pre.clone(),
            held: self.held.clone(),
            bodies: self.bodies.clone(),
            remaps: self.remaps.clone(),
            words: self.words.borrow().clone(),
            coded_held: Arc::new(self.coded_held.borrow().clone()),
            once: self.once.clone(),
            shares_modules: self.enc.shares_modules(),
            paks: self.paks.clone(),
        }
    }
}

impl Apart {
    /// A context of data of `n` bytes.
    fn cx(&self, n: usize, opt: &Options) -> Cx {
        let mut own = Cx::of_len(n, opt);
        if self.shares_modules {
            own.enc.share_modules();
        }
        own.pre = self.pre.clone();
        own.held = self.held.clone();
        own.bodies = self.bodies.clone();
        own.remaps = self.remaps.clone();
        own.share_words(self.words.clone());
        own.coded_held = RefCell::new((*self.coded_held).clone());
        own.once = self.once.clone();
        own.paks = self.paks.clone();
        own
    }
}

/// Whether formula `f` of a content takes the place of its slice ([`nested`]): one of
/// applications, or aqr-ppmd's, which only cuts the header off the data, so that the data is
/// stored once where another file starts with it, or aqr-mkv's, which holds frames seen
/// before once, or a gzip or zlib stream's below level 4, which cuts its DEFLATE stream off for
/// files made of it ([`inflated::is`]).
fn takes_place(f: &Formula) -> bool {
    !f.applies().is_empty() || aqr_ppmd::formula::is(f) || aqr_mkv::formula::is(f) || aqueous::is(f) || inflated::is(f)
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
    // (the applications of several outputs [`nested`] looked into, not those of the sets)
    let own = f.shared.len();
    let mut outputs: Vec<Option<Node>> = (0..children.len()).map(|_| None).collect();
    for s in sets {
        let Node::Concat(outs) = f.embed(s.f) else { unreachable!("a set's root is its outputs") };
        s.members.into_iter().zip(outs).for_each(|(m, o)| outputs[m] = Some(o));
    }
    let children = children.into_iter().zip(outputs).map(|(c, o)| match o {
        Some(o) => Some(Placed::Node(o)),
        None => c.map(Placed::Made),
    });
    // (of the applications of several outputs too)
    let mut slices = vec![0; f.streams.len()];
    f.visit(&mut |n| {
        if let &Node::Slice { stream, .. } = n
            && let Some(n) = slices.get_mut(stream)
        {
            *n += 1;
        }
    });
    let mut children = children.into_iter();
    // (the applications embedded in the inputs of one of them go before it, as its inputs take
    // their outputs)
    let mut order: Vec<usize> = (own..f.shared.len()).collect();
    for i in 0..own {
        let embedded = f.shared.len();
        // (a patch's source and patch, of contents kept as they are: nothing placed in them)
        if let Some(module) = f.shared[i].apply().map(|a| a.module) {
            let inputs = std::mem::take(&mut f.shared[i].apply_mut().expect("an application").inputs);
            let mut n = Node::apply(module, Vec::new(), inputs, 0);
            place(&mut n, f, &slices, opt, &mut children);
            let Node::Apply(a) = n else { unreachable!("an application") };
            f.shared[i].apply_mut().expect("an application").inputs = a.inputs;
        } else if let aqr_formula::Of::Node(n) = &mut f.shared[i].of {
            // (a node split: what is placed in it is cut into the same outputs)
            let mut n = std::mem::replace(n, Node::Concat(Vec::new()));
            place(&mut n, f, &slices, opt, &mut children);
            f.shared[i].of = aqr_formula::Of::Node(n);
        }
        order.extend(embedded..f.shared.len());
        order.push(i);
    }
    let mut root = std::mem::replace(&mut f.root, Node::Concat(Vec::new()));
    let embedded = f.shared.len();
    place(&mut root, f, &slices, opt, &mut children);
    f.root = root;
    order.extend(embedded..f.shared.len());
    f.reorder_shared(&order);
    exe::resolve(f);
    f.compact();
}

/// The applications of several outputs of `f`, as nodes: what is below them comes before the
/// root, wherever contents are looked for.
fn shared_nodes(f: &Formula) -> Vec<Node> {
    use aqr_formula::{Of, Patch};
    let node = |s: &aqr_formula::Shared| match &s.of {
        Of::Apply(a) => Node::Apply(Box::new(a.clone())),
        Of::Patch { inputs: [source, patch], len } => {
            Node::Patch(Box::new(Patch { source: source.clone(), patch: patch.clone(), len: *len }))
        }
        Of::Node(n) => n.clone(),
    };
    f.shared.iter().map(node).collect()
}

/// [`description_bound`] of all of `f`: its root, and its applications of several outputs with
/// their sizes.
fn bound_of(f: &Formula) -> usize {
    let shared = f.shared.iter().zip(shared_nodes(f)).map(|(s, n)| {
        let sizes: usize = s.sizes.iter().map(|&n| aqr_common::varint::len(n)).sum();
        description_bound(&n) + 1 + aqr_common::varint::len(s.sizes.len() as u64) + sizes
    });
    description_bound(&f.root) + shared.sum::<usize>()
}

/// The contents below node `n` of `f`, in order: each slice, and its formula where it has any
/// application: a slice of a stream that is [`Stream::content`] ([`Formula::mark_contents`]),
/// scanned. `apart` is [`bundle::contents_apart`] of `f`.
fn contents(
    n: &Node,
    f: &Formula,
    apart: bool,
    opt: &Options,
    depth: u32,
    cx: &Cx,
    out: &mut Vec<(Node, Option<Made>)>,
) {
    match n {
        &Node::Slice { stream, .. } if is_content(f, stream) => {
            content_slices(n, Nest::Scan, f, apart, opt, depth, cx, out)
        }
        // (patches are made of recompressed data, not looked into)
        Node::Slice { .. } | Node::Output { .. } | Node::Patch(_) | Node::ModuleBytes(..) | Node::ResourceBytes(_) => {}
        Node::Concat(ns) | Node::Pack(ns) => ns.iter().for_each(|n| contents(n, f, apart, opt, depth, cx, out)),
        Node::Apply(a) => a.inputs.iter().for_each(|n| contents(n, f, apart, opt, depth, cx, out)),
    }
}

/// [`contents`] of node `n`, a content recompressed as `nest`: each of its slices is a content
/// of the kind of its stream, and the applications in it have contents of their own.
#[allow(clippy::too_many_arguments)]
fn content_slices(
    n: &Node,
    nest: Nest,
    f: &Formula,
    apart: bool,
    opt: &Options,
    depth: u32,
    cx: &Cx,
    out: &mut Vec<(Node, Option<Made>)>,
) {
    match n {
        &Node::Slice { stream, start, len } => {
            let s = f.streams.get(stream);
            // (data not made yet is made as it is come to, [`make_later`])
            if let Some(st) = s.filter(|s| s.data.is_lazy())
                && start + len <= st.data.len() as u64
                && apart
                && let Some(nest) = nest.of(st.kind)
            {
                cx.later.borrow_mut().push((out.len(), n.clone(), nest));
                out.push((n.clone(), None));
                return;
            }
            let d = s.filter(|s| !s.data.is_lazy()).and_then(|s| s.data.get(start as usize..)?.get(..len as usize));
            // (the first of a set's contents with a parent: made in the set; any other alone)
            if d.is_some_and(|d| cx.skip.borrow_mut().remove(&(d.as_ptr() as usize, d.len()))) {
                out.push((n.clone(), None));
                return;
            }
            // (the files of an archive made later, at once, and so are those of an image or a
            // WIM file, and a ZIP archive's entries)
            if (depth == 0 && cx.top_sets.is_some() || apart)
                && let Some(nest) = s.and_then(|s| nest.of(s.kind)).filter(|_| d.is_some())
            {
                cx.later.borrow_mut().push((out.len(), n.clone(), nest));
                out.push((n.clone(), None));
                return;
            }
            let child = s.zip(d).and_then(|(s, d)| made(d, opt, depth + 1, nest.of(s.kind)?, cx));
            out.push((n.clone(), child.filter(|c| takes_place(&c.f))));
        }
        Node::Concat(ns) | Node::Pack(ns) => {
            ns.iter().for_each(|n| content_slices(n, nest, f, apart, opt, depth, cx, out))
        }
        Node::Apply(_) => contents(n, f, apart, opt, depth, cx, out),
        Node::Output { .. } | Node::Patch(_) | Node::ModuleBytes(..) | Node::ResourceBytes(_) => {}
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
        Node::Slice { .. } | Node::Output { .. } | Node::Patch(_) | Node::ModuleBytes(..) | Node::ResourceBytes(_) => {}
        Node::Concat(ns) | Node::Pack(ns) => ns.iter_mut().for_each(|n| place(n, f, slices, opt, children)),
        Node::Apply(a) => a.inputs.iter_mut().for_each(|n| place(n, f, slices, opt, children)),
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
                // (one cut already, out of the bytes it took, takes none: [`make_later`])
                let child = if child.gaps.is_none() {
                    child.f
                } else if slices[stream] == 1 && start == 0 && len == data.len() as u64 {
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
        Node::Output { .. } | Node::Patch(_) | Node::ModuleBytes(..) | Node::ResourceBytes(_) => {}
    }
}

/// Whether `stream` of `f` is [`Stream::content`].
fn is_content(f: &Formula, stream: usize) -> bool {
    f.streams.get(stream).is_some_and(|s| s.content)
}

/// What the recompressor of a [`pure`] format is given besides the data: the options (of the
/// level and the threads), how many formats deep the data is, and what the context lends.
pub(crate) struct Pure<'a> {
    opt: &'a Options,
    depth: u32,
    /// The dictionary text is coded with, if any ([`Cx::words`]).
    words: Option<&'a aqr_wrt::formula::Shared>,
    /// Which text is coded once ([`Cx::once`]).
    once: &'a Once,
    /// What the files archived along hold ([`Cx::held`]).
    held: Option<&'a Held>,
    /// Where the formula is of the file itself archived directly ([`direct_room`]): the order
    /// whose sink the formula's parts are sealed into as they are made, as the recompressor may
    /// have them ([`tensors`], [`Pure::made`]).
    seal: Option<&'a seal::Order>,
    /// Where those parts are evaluated beforehand ([`Cx::pre`]).
    pre: Option<&'a verify::Prevalidator>,
}

impl Pure<'_> {
    /// What part `part` of the formula, of bytes `of`, makes ready to be sealed into the sink of
    /// [`Pure::seal`], on any thread: its memory declared, evaluated beforehand, and its blocks
    /// of its own encoded ([`aqr::sink::Sink::ready`]).
    fn made(&self, part: &mut Formula, of: &[u8]) -> aqr::sink::Ready {
        let order = self.seal.expect("sealing");
        let _a = aqr_common::allowance::enter(aqr_common::allowance::Context::NONE);
        memory::declare(part, &Native { threads: 1 }, &memory::Limits::default());
        if let Some(pre) = self.pre {
            part.streams.iter_mut().for_each(|s| s.data.share());
            pre.submit(Arc::new(part.clone()), of);
        }
        order.sink().ready(part).expect("encoded")
    }
}

/// aqr-tensors' formula of `d`, each segment coded sealed as it is made where `p` tells, else its
/// streams spilled.
fn tensors(d: &[u8], p: &Pure) -> Option<Formula> {
    let segment = aqr_tensors::formula::SEGMENT;
    #[cfg(test)]
    let segment = tests::TENSORS_SEGMENT.get().unwrap_or(segment);
    match p.seal {
        Some(order) => {
            let made = |part: &mut Formula, of: &[u8]| p.made(part, of);
            let mut seal = |part: &mut Formula, _: &[u8], ready| order.sink().seal_ready(part, ready).expect("sealed");
            aqr_tensors::formula::encode_sealing(d, p.opt.threads, segment, &made, &mut seal)
        }
        None => aqr_tensors::formula::encode_in(d, p.opt.threads, segment),
    }
}

/// The formula that the recompressor of `format` makes of `d` (its `formula` module, see its
/// FORMAT.md), if it makes one, where it is a function of `d` alone (it looks for no files in
/// its data, and takes nothing of a budget) and of what `p` gives: `None` for the other formats
/// ([`Recompressor::Cx`]). Whether coding pays off is the recompressor's call: the streams are
/// not compressed yet, so their size says little. The formula is the same whatever the threads.
/// Text is coded as `p.once` tells.
fn pure(format: Format, d: &[u8], p: &Pure) -> Option<Option<Formula>> {
    let r = recompressor(format);
    if let Recompressor::Cx(_) = r {
        return None;
    }
    if !fits(format, d.len()) {
        return Some(None);
    }
    #[cfg(test)]
    tests::PURE.with(|n| n.set(n.get() + 1));
    let code = || {
        #[cfg(test)]
        if format == Format::Text {
            tests::TEXTS.lock().unwrap().push(d.len());
        }
        catch_unwind(AssertUnwindSafe(|| {
            attempted(format, d, || match r {
                Recompressor::Pure(e) => e(d, p.opt.threads),
                Recompressor::PureWith(e) => e(d, p),
                Recompressor::Cx(_) => None,
            })
        }))
        .ok()
        .flatten()
    };
    Some(if format == Format::Text { p.once.code(d, code) } else { code() })
}

/// What `make`, the recompressor of `format` on `d`, makes, within the heap one recompression
/// may take ([`aqr_common::allowance::attempt`] of [`aqr_common::budget::attempt_limit`], which
/// those it makes of the contents share): `None` where it gave up past it, which is told
/// ([`fallbacks`]), as if it did not take the data.
fn attempted(format: Format, d: &[u8], make: impl FnOnce() -> Option<Formula>) -> Option<Formula> {
    let limit = aqr_common::budget::attempt_limit();
    #[cfg(test)]
    let (limit, hog) = tests::hog(format, d.len()).map_or((limit, false), |l| (l, true));
    let make = || {
        // (more than the limit taken at the start, a reservation of no pages touched, and the
        // check after it, as the next item on the pool would)
        #[cfg(test)]
        let _hog = hog.then(|| {
            let v = Vec::<u8>::with_capacity(limit as usize + 1);
            aqr_common::allowance::check();
            v
        });
        make()
    };
    match aqr_common::allowance::attempt(limit, make) {
        Ok(f) => f,
        Err(e) => {
            memory::gave_up_heap(format.name(), e.used, d.len() as u64);
            None
        }
    }
}

/// aqr-bz2's formula of `d` (`depth` formats deep), each stream taken only where its data may
/// pay ([`probe`]; `held` telling what the files archived with it hold): not compressed again
/// to check it where it would not pay, which costs most (some 12 MB/s of data a thread), nor
/// decoded past its start (50-80 MB/s) where that start tells. Data that starts as a file of a
/// format (a tar archive, say), whose files further on or what they share the start does not
/// tell, is decoded whole first; other data (text, say) is told by its start. The record is the
/// params, a few bytes.
fn bz2(d: &[u8], opt: &Options, depth: u32, threads: usize, held: Option<&Held>) -> Option<Formula> {
    aqr_bz2::formula::encode_if(d, threads, &mut bz2_pays(d, opt, depth, held))
}

/// Whether a bzip2 stream in file `d` pays, as aqr-bz2's `encode_if` asks: see [`bz2`].
fn bz2_pays<'a>(
    d: &'a [u8],
    opt: &Options,
    depth: u32,
    held: Option<&'a Held>,
) -> impl FnMut(usize, &[u8], bool) -> bool + 'a {
    let probe = probe(opt, depth);
    // (an encoder of its own, as formulas of pure formats are made on several threads)
    let enc = aqr::Encoder::new(opt.level);
    move |coded, data, whole| {
        if !whole && detect(data).is_some_and(|f| f != Format::Text) {
            return true;
        }
        probe.as_ref().is_none_or(|p| {
            let held = || held.is_some_and(|h| h.holds(data, h.owner(d))) as u8 as f64;
            p.pays(&enc, coded, || Some(16), data, held, Kind::Unknown)
        })
    }
}

/// aqr-alz's formula of an ALZ archive, the contents of its entries a content of the
/// applications (or of the formula, those stored), recompressed in turn, each by its kind by its
/// name ([`kind_by_name`]). Its DEFLATE and bzip2 bodies decode within what is left of the
/// budget ([`Options::expansion`]); a bzip2 body is taken where its data may pay, as a bzip2
/// file is ([`bz2`]).
fn alz(d: &[u8], opt: &Options, depth: u32, cx: &Cx) -> Option<Formula> {
    let (profile, zlib) = opt.flate();
    let o =
        aqr_alz::formula::Options { threads: opt.threads, profile, zlib, limit: cx.inflate_left(), kind: kind_by_name };
    let e = aqr_alz::formula::encode_with(d, &o, &mut bz2_pays(d, opt, depth, cx.held.as_deref()))?;
    cx.inflate(e.inflated);
    Some(e.formula)
}

/// aqr-isc's formula of an InstallShield cabinet, the contents of its files a content of the
/// application, recompressed in turn. Its chunks decode within what is left of the budget
/// ([`Options::expansion`]).
fn isc(d: &[u8], opt: &Options, cx: &Cx) -> Option<Formula> {
    let (profile, zlib) = opt.flate();
    let o = aqr_isc::formula::Options { threads: opt.threads, profile, zlib, limit: cx.inflate_left() };
    let e = aqr_isc::formula::encode_with(d, &o)?;
    cx.inflate(e.inflated);
    Some(e.formula)
}

/// aqr-egg's formula of an EGG archive, the contents of its files a content of the
/// applications (or of the formula, those stored), recompressed in turn, each by its kind by its
/// name ([`kind_by_name`]). Its DEFLATE, bzip2 and LZMA blocks decode within what is left of the
/// budget ([`Options::expansion`]); a bzip2 block is taken where its data may pay, as a bzip2
/// file is ([`bz2`]).
fn egg(d: &[u8], opt: &Options, depth: u32, cx: &Cx) -> Option<Formula> {
    let (profile, zlib) = opt.flate();
    let o =
        aqr_egg::formula::Options { threads: opt.threads, profile, zlib, limit: cx.inflate_left(), kind: kind_by_name };
    let e = aqr_egg::formula::encode_with(d, &o, &mut bz2_pays(d, opt, depth, cx.held.as_deref()))?;
    cx.inflate(e.inflated);
    Some(e.formula)
}

/// aqr-lha's formula of an LHA archive, the contents of its files a content of the application
/// (or of the formula, those stored), recompressed in turn, each by its kind by its name
/// ([`kind_by_name`]). Its streams decode within what is left of the budget
/// ([`Options::expansion`]).
fn lha(d: &[u8], opt: &Options, cx: &Cx) -> Option<Formula> {
    let o = aqr_lha::formula::Options { threads: opt.threads, limit: cx.inflate_left(), kind: kind_by_name };
    let e = aqr_lha::formula::encode_with(d, &o)?;
    cx.inflate(e.inflated);
    Some(e.formula)
}

/// aqr-rar's formula of a RAR archive, the contents of its files a content of the application
/// (or of the formula, those stored), recompressed in turn, each by its kind by its name
/// ([`kind_by_name`]). Its streams decode within what is left of the budget
/// ([`Options::expansion`]); those with PPMd blocks only from level 9 ([`ppmd_on`]).
fn rar(d: &[u8], opt: &Options, cx: &Cx) -> Option<Formula> {
    let o = aqr_rar::formula::Options {
        threads: opt.threads,
        limit: cx.inflate_left(),
        kind: kind_by_name,
        ppmd: ppmd_on(opt),
    };
    let e = aqr_rar::formula::encode_with(d, &o)?;
    cx.inflate(e.inflated);
    Some(e.formula)
}

/// aqr-7z's formula, the contents of the files a content of the applications (or of the formula,
/// those of stored folders), recompressed in turn, each by its kind by its name
/// ([`kind_by_name`]).
fn sevenz(d: &[u8], opt: &Options) -> Option<Formula> {
    let o = aqr_7z::formula::Options { threads: opt.threads, kind: kind_by_name, ..Default::default() };
    aqr_7z::formula::encode_with(d, &o)
}

/// aqr-mszip's formula of a CAB file, the contents of the files a content of the applications,
/// recompressed in turn, each by its kind by its name ([`kind_by_name`]). Its MSZIP and LZX
/// folders (the latter from level 9, [`lzx_on`]) inflate within what is left of the budget
/// ([`Options::expansion`]).
fn cab(d: &[u8], opt: &Options, cx: &Cx) -> Option<Formula> {
    let (profile, zlib) = opt.flate();
    let o = aqr_mszip::formula::Options {
        threads: opt.threads,
        profile,
        zlib,
        limit: cx.inflate_left(),
        kind: kind_by_name,
        lzx: lzx_on(opt),
    };
    let e = aqr_mszip::formula::encode_with(d, &o)?;
    cx.inflate(e.inflated);
    Some(e.formula)
}

/// aqr-mszip's formula of a CHM file, the files of its LZX section a content of the application,
/// recompressed in turn, each by its kind by its name; within what is left of the budget, from
/// level 9 ([`lzx_on`], [`format_on`]).
fn chm(d: &[u8], opt: &Options, cx: &Cx) -> Option<Formula> {
    let o = aqr_mszip::formula::Options {
        threads: opt.threads,
        limit: cx.inflate_left(),
        kind: kind_by_name,
        ..Default::default()
    };
    let e = aqr_mszip::formula::encode_chm_with(d, &o)?;
    cx.inflate(e.inflated);
    Some(e.formula)
}

/// aqr-pdf's options at the level of `opt`: the records of its DEFLATE streams of the profile
/// that [`Options::flate`] tells, and from level 7 on with no time budget, as DEFLATE is always
/// rebuilt there ([`probe`]).
fn pdf_options(opt: &Options, threads: usize) -> aqr_pdf::encode::Options {
    let (profile, zlib) = opt.flate();
    let default = aqr_pdf::encode::Options::default();
    let speed = if opt.level.scale() >= 7 { f64::INFINITY } else { default.speed };
    aqr_pdf::encode::Options { speed, profile, zlib, threads, ..default }
}

/// Whether `format` is one of [`pure`].
fn is_pure(format: Format) -> bool {
    !matches!(recompressor(format), Recompressor::Cx(_))
}

/// How the recompressor of a format is run.
#[derive(Clone, Copy)]
pub(crate) enum Recompressor {
    /// A [`pure`] one, its `encode`: the formula of the data, on so many threads.
    Pure(fn(&[u8], usize) -> Option<Formula>),
    /// A [`pure`] one taking more of what [`Pure`] gives than the threads.
    PureWith(fn(&[u8], &Pure) -> Option<Formula>),
    /// One that looks for files in its data or takes of a budget, given the context: of the data,
    /// the kind of its contents (of a zlib stream or gzip member without a name), the options and
    /// how many formats deep it is.
    Cx(fn(&[u8], Kind, &Options, u32, &Cx) -> Option<Formula>),
}

/// The formula of a disk image: of a compressed container by its chunks, its disk a content (as a
/// DMG); else of its file systems, the Splits of files in pieces recorded, their contents at
/// hand (see `chunked`).
fn disk(d: &[u8], opt: &Options, cx: &Cx) -> Option<Formula> {
    if let Some(f) = chunked::container(d, opt, cx) {
        return Some(f);
    }
    let f = bundle::disk(d, opt.threads)?;
    if let Some(pre) = &cx.pre {
        pre.learn_splits(&f, opt.threads);
    }
    Some(f)
}

/// The recompressor of `format` ([`formats::Spec::recompressor`]).
fn recompressor(format: Format) -> Recompressor {
    format.spec().recompressor
}

/// Whether recompressing `len` bytes as `format` takes no more heap by its estimate
/// ([`formats::Spec::heap`]) than one recompression may ([`aqr_common::budget::attempt_limit`]: what
/// `aqr -M` asks for, else the budget of the process or half the system's memory where more),
/// which it is then held to as it runs ([`attempted`]). The estimate turns down up front what
/// would give up after much of the work. (With `AQR_TEST_NO_ESTIMATE` set and not empty,
/// everything fits, so that what is held to the limit as it runs can be seen.)
fn fits(format: Format, len: usize) -> bool {
    static NO_ESTIMATE: std::sync::LazyLock<bool> =
        std::sync::LazyLock::new(|| std::env::var_os("AQR_TEST_NO_ESTIMATE").is_some_and(|v| !v.is_empty()));
    *NO_ESTIMATE || fits_within(format, len, aqr_common::budget::attempt_limit())
}

fn fits_within(format: Format, len: usize, limit: u64) -> bool {
    format.spec().heap * len as f64 <= limit as f64
}

/// Makes the formulas of the files of [`pure`] formats among the contents of `f` beforehand,
/// at once (as [`contents`] would take them in turn), into [`Cx::ready`]; returns their keys,
/// and the plan of the sets of the PNG files among them ([`sets::plan`]), whose members with
/// a parent are not made but left out of the contents ([`Cx::skip`]).
fn prepare(f: &Formula, opt: &Options, depth: u32, cx: &Cx) -> (Vec<(usize, usize, Format)>, sets::Plan) {
    let mut jobs = Vec::new();
    for n in shared_nodes(f).iter().chain([&f.root]) {
        pure_contents(n, f, opt, &mut jobs);
    }
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
    // (each may take all of `opt.threads`: the pool gives it the workers free, and those given
    // back as the others finish)
    let words = cx.words.borrow().clone();
    let p =
        Pure { opt, depth, words: words.as_deref(), once: &cx.once, held: cx.held.as_deref(), seal: None, pre: None };
    let files = cx.files.as_deref();
    let made = sets::par_map(jobs.len(), workers, |i| {
        files.inspect(|f| f.tell(jobs[i].1.as_ptr(), false));
        let _item = aqr_common::trace::item(files.and_then(|f| f.content(jobs[i].1.as_ptr())));
        let mut made = pure(jobs[i].0, jobs[i].1, &p).flatten();
        // (of the store as it is made, as [`recompress_with`] has a formula: they are all made
        // before any is taken)
        made.iter_mut().for_each(aqr_formula::store::spill_streams);
        files.inspect(|f| f.tell(jobs[i].1.as_ptr(), true));
        made
    });
    let mut ready = cx.ready.borrow_mut();
    let keys: Vec<_> = jobs.iter().map(|&(format, d, _)| (d.as_ptr() as usize, d.len(), format)).collect();
    for (&k, f) in keys.iter().zip(made) {
        ready.insert(k, f);
    }
    (keys, plan)
}

/// The files of [`pure`] formats below node `n` of `f` that [`contents`] would take as such
/// (those of a kind of the format, or that a scan finds at the start), each with its data as it
/// is given its recompressor (to its [`extent`]), and its slice where that is all of the content.
fn pure_contents<'a>(n: &Node, f: &'a Formula, opt: &Options, out: &mut Vec<(Format, &'a [u8], Option<sets::At>)>) {
    let mut found = Vec::new();
    pure_candidates(n, f, &mut found);
    // (the scans at once: of an image's thousands of files, they took a second alone)
    let found = aqr_common::pool::par_map(found.len(), opt.threads, |i| {
        let (nest, d, at) = found[i];
        // (a scan takes the file it starts with as one of the format it starts like)
        let format = match nest {
            Nest::As(format) => Some(format),
            Nest::Scan => detect(d),
            _ => None,
        };
        let format = format.filter(|&f| is_pure(f) && format_on(f, opt))?;
        let n = extent(format, d).filter(|&n| n > 0)?;
        Some((format, &d[..n], (n == d.len()).then_some(at)))
    });
    out.extend(found.into_iter().flatten());
}

/// The slices [`pure_contents`] looks at below node `n` of `f`, each with how it is taken (its
/// stream's kind's nest) and its data.
fn pure_candidates<'a>(n: &Node, f: &'a Formula, out: &mut Vec<(Nest, &'a [u8], sets::At)>) {
    match n {
        &Node::Slice { stream, start, len } if is_content(f, stream) => {
            let Some(s) = f.streams.get(stream).filter(|s| !s.data.is_lazy()) else { return };
            let Some(d) = s.data.get(start as usize..).and_then(|d| d.get(..len as usize)) else { return };
            if let Some(nest) = Nest::Scan.of(s.kind) {
                out.push((nest, d, (stream, start, len)));
            }
        }
        Node::Concat(ns) | Node::Pack(ns) => ns.iter().for_each(|n| pure_candidates(n, f, out)),
        Node::Apply(a) => a.inputs.iter().for_each(|n| pure_candidates(n, f, out)),
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
    use aqr_decompress::FLATE;

    thread_local! {
        /// The formats given to recompressors, in order.
        pub(crate) static TRIED: std::cell::RefCell<Vec<Format>> = const { std::cell::RefCell::new(Vec::new()) };
        /// The DEFLATE streams of gzip and zlib deconstructed.
        pub(crate) static DECONSTRUCTED: Cell<usize> = const { Cell::new(0) };
        /// The files of [`pure`] formats given to their recompressors.
        pub(crate) static PURE: Cell<usize> = const { Cell::new(0) };
        /// The lengths of the data compared with their formulas by [`smaller`].
        pub(crate) static COMPARED: std::cell::RefCell<Vec<usize>> = const { std::cell::RefCell::new(Vec::new()) };
        /// [`direct_min`] of the tests, where set.
        pub(crate) static DIRECT_MIN: Cell<Option<usize>> = const { Cell::new(None) };
        /// The room [`direct_room`] leaves, where set.
        pub(crate) static DIRECT_ROOM: Cell<Option<u64>> = const { Cell::new(None) };
        /// The archives written directly ([`written_direct`]).
        pub(crate) static DIRECTED: Cell<usize> = const { Cell::new(0) };
        /// The bytes of a segment of aqr-tensors ([`tensors`]), where set.
        pub(crate) static TENSORS_SEGMENT: Cell<Option<usize>> = const { Cell::new(None) };
    }

    /// The lengths of the texts given to aqr-wrt, on any thread.
    pub(crate) static TEXTS: std::sync::Mutex<Vec<usize>> = std::sync::Mutex::new(Vec::new());

    // (an allocator that tells the heap of recompressions to their allowances, as the binaries'
    // does: the system's, as some tests here expect of it)
    #[global_allocator]
    static ALLOC: aqr_common::mem::Counting = aqr_common::mem::Counting;

    /// The recompressor of a format, given data of a length, takes more heap than one may at
    /// its start ([`hog`]), on any thread.
    static HOGS: std::sync::Mutex<Vec<(Format, usize)>> = std::sync::Mutex::new(Vec::new());

    /// The heap that one recompression told of [`HOGS`] may take.
    const HOG_LIMIT: u64 = 16 << 20;

    /// Held by the tests that look at [`fallbacks`], of the process: one at a time.
    pub(crate) static FALLBACKS_SEEN: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// The heap a recompression as `format` of data of `len` bytes may take, where it takes
    /// more at its start ([`HOGS`], [`attempted`]).
    pub(crate) fn hog(format: Format, len: usize) -> Option<u64> {
        HOGS.lock().unwrap().contains(&(format, len)).then_some(HOG_LIMIT)
    }

    /// A recompression that takes more heap than one may gives up, and the data is kept as it
    /// is (the archive giving it back), what gave up told ([`fallbacks`]); a recompression
    /// within it (of its contents) that does so gives up alone.
    #[test]
    fn heap_over_falls_back() {
        let text: Vec<u8> =
            (0..30011).flat_map(|i| format!("line {} of the hogging text\n", i * 7 % 1000).into_bytes()).collect();
        let gz = |t: &[u8]| {
            let mut z = flate2::GzBuilder::new().filename("text.txt").write(Vec::new(), flate2::Compression::new(6));
            std::io::Write::write_all(&mut z, t).unwrap();
            z.finish().unwrap()
        };
        let outer = gz(&text);
        let _seen = FALLBACKS_SEEN.lock().unwrap_or_else(|e| e.into_inner());
        let opt = Options { smaller: false, ..Options::default() };
        let made = |d: &[u8]| {
            let f = recompress(d, &opt);
            assert!(f.eval(&Native::default()).is_ok_and(|b| b == d));
            f.applies().iter().map(|a| a.module.name).collect::<Vec<_>>()
        };
        let mine = |told: Vec<Fallback>, len: usize| -> Vec<Fallback> {
            told.into_iter().filter(|f| f.heap && f.bytes == len as u64).collect()
        };
        assert!(!made(&outer).is_empty());
        HOGS.lock().unwrap().push((Format::Gzip, outer.len()));
        assert!(made(&outer).is_empty(), "kept as it is");
        let told = mine(fallbacks(), outer.len());
        assert!(told.len() == 1 && told[0].module == "gzip" && told[0].needed > HOG_LIMIT, "{told:?}");
        // (the text in it given up alone: the gzip stream deconstructed still)
        HOGS.lock().unwrap().retain(|h| h.1 != outer.len());
        HOGS.lock().unwrap().push((Format::Text, text.len()));
        let modules = made(&outer);
        assert!(!modules.is_empty() && !modules.iter().any(|m| m.contains("wrt")), "{modules:?}");
        let told = mine(fallbacks(), text.len());
        assert!(told.len() == 1 && told[0].module == "text", "{told:?}");
        HOGS.lock().unwrap().clear();
    }

    /// An ar, tar or cpio archive is worth what the files in it are: a probe counts them, not
    /// the archive, and scans data of its kind.
    #[test]
    fn bundles_probed_by_members() {
        let fixture = |p: &str| std::fs::read(format!("{}/../../{p}", env!("CARGO_MANIFEST_DIR"))).unwrap();
        let tar = fixture("codecs/aqr-bundle/tests/data/py-gnu.tar");
        assert_eq!(files_len(&tar), 0);
        let jpeg = fixture("codecs/aqr-jpeg/tests/data/gradient.jpg");
        let mut with = tar[..1024].to_vec();
        with.extend_from_slice(&jpeg);
        with.extend_from_slice(&tar[1024..]);
        assert_eq!(files_len(&with), jpeg.len());
        let probe = probe(&Options::default(), 0).unwrap();
        assert_eq!((probe.look)(Kind::Format("tar")), Look::Scan);
        assert_eq!((probe.look)(Kind::Format("jpeg")), Look::Whole);
    }

    /// An ISO 9660 or UDF image is taken apart into its files, recompressed in turn: a JPEG file
    /// in it by aqr-jpeg.
    #[test]
    fn images_taken_apart() {
        let fixture = |p: &str| std::fs::read(format!("{}/../../{p}", env!("CARGO_MANIFEST_DIR"))).unwrap();
        let z = fixture("codecs/aqr-iso/tests/data/udf201.iso.zst");
        let mut d = zstd::stream::decode_all(&z[..]).unwrap();
        let jpeg = fixture("codecs/aqr-jpeg/tests/data/gradient.jpg");
        let files = aqr_disk::files(&d, bundle::FILE_SYSTEMS);
        let at = files.iter().find(|f| f.name == b"bin/data.bin").unwrap().extents[0].start;
        d[at..at + jpeg.len()].copy_from_slice(&jpeg);
        assert_eq!(detect(&d), Some(Format::Disk));
        // (the modules the archive's anyway)
        let f = recompress(&d, &Options { shared_modules: true, ..Options::default() });
        assert!(f.streams.iter().any(|s| s.class == aqr_disk::formula::REST));
        assert!(f.applies().iter().any(|a| a.module == aqr_decompress::JPEG));
        assert!(f.eval(&Native::default()).unwrap() == d);
        // the files of an image known to the store (mapped) shared, not copied: the same archive
        let plain = compress(&d, &Options::default());
        let shared = aqr_formula::Bytes::shared(Arc::new(d));
        let _known = aqr_formula::store::known(&shared);
        let f = bundle::disk(&shared, 1).unwrap();
        let large: Vec<_> = f.streams.iter().filter(|s| s.content && s.data.len() >= aqr_formula::store::MIN).collect();
        assert!(!large.is_empty() && large.iter().all(|s| s.data.is_shared()));
        assert!(compress(&shared, &Options::default()) == plain);
        // a file all of its stream takes its bytes, cut out of them as it is made (let go of
        // then): the JPEG file's stream empty, its formula of no gaps left
        let mut f = bundle::disk(&shared, 1).unwrap();
        let opt = Options { shared_modules: true, ..Options::default() };
        let cx = Cx::new(&shared, &opt);
        let (children, _) = nested(&mut f, &shared, &opt, 0, &cx).unwrap();
        let (n, child) = children.iter().find(|c| c.1.as_ref().is_some_and(|c| !c.f.applies().is_empty())).unwrap();
        let &Node::Slice { stream, .. } = n else { panic!("a slice") };
        assert!(f.streams[stream].data.is_empty() && child.as_ref().unwrap().gaps.is_none());
    }

    /// A WIM file whose bytes are known to the store (mapped) has its data made as each content
    /// is come to rather than held ([`wim`]): the same archive as where it is not, giving the
    /// file back.
    #[test]
    fn wim_data_made_as_needed() {
        let d =
            std::fs::read(format!("{}/../../codecs/aqr-wim/tests/data/lzx.wim", env!("CARGO_MANIFEST_DIR"))).unwrap();
        let opt = Options { threads: 2, ..Options::default() };
        let plain = compress(&d, &opt);
        let shared = aqr_formula::Bytes::shared(Arc::new(d.clone()));
        let _known = aqr_formula::store::known(&shared);
        assert!(wim(&shared, 2).unwrap().streams.iter().any(|s| s.data.is_lazy()));
        assert!(compress(&shared, &opt) == plain);
        assert!(aqr_decompress::decompress(&plain, 1).unwrap() == d);
    }

    /// `d` as a file mapped (of a file made for it and gone), and the bytes of the mapping
    /// resident in the process (Linux: its `Rss` in `/proc/self/smaps`).
    #[cfg(target_os = "linux")]
    fn mapped(d: &[u8]) -> (aqr_formula::Bytes, impl Fn(&aqr_formula::Bytes) -> usize) {
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("aqr-compress-mapped-{}-{n}", std::process::id()));
        std::fs::write(&path, d).unwrap();
        // SAFETY: a file of this test's alone, not changed while mapped
        let map = unsafe { memmap2::Mmap::map(&std::fs::File::open(&path).unwrap()) }.unwrap();
        std::fs::remove_file(&path).unwrap();
        let rss = |b: &aqr_formula::Bytes| {
            let at = b.as_ptr() as usize;
            let smaps = std::fs::read_to_string("/proc/self/smaps").unwrap();
            let mut lines = smaps.lines().skip_while(|l| {
                let Some((a, b)) = l.split(' ').next().and_then(|r| r.split_once('-')) else { return true };
                let (a, b) = (usize::from_str_radix(a, 16), usize::from_str_radix(b, 16));
                !matches!((a, b), (Ok(a), Ok(b)) if (a..b).contains(&at))
            });
            lines.next().expect("the mapping");
            let rss = lines.find_map(|l| l.strip_prefix("Rss:")).expect("its Rss");
            rss.trim().trim_end_matches(" kB").trim().parse::<usize>().unwrap() << 10
        };
        (aqr_formula::Bytes::shared(Arc::new(map)), rss)
    }

    /// Every page of `b` read.
    #[cfg(target_os = "linux")]
    fn touch(b: &[u8]) {
        b.iter().step_by(4096).for_each(|&x| _ = std::hint::black_box(x));
    }

    /// The pages of an input mapped are let go of as it is read through (its contents sealed, its
    /// streams encoded, its bytes compared in verifying): not resident as compressing ends,
    /// the archive the same.
    #[cfg(target_os = "linux")]
    #[test]
    fn input_let_go() {
        let fixture = |p: &str| std::fs::read(format!("{}/../../{p}", env!("CARGO_MANIFEST_DIR"))).unwrap();
        let d = zstd::stream::decode_all(&fixture("codecs/aqr-iso/tests/data/udf201.iso.zst")[..]).unwrap();
        let (mapped, rss) = mapped(&d);
        let opt = Options { threads: 2, ..Options::default() };
        let plain = compress(&d, &opt);
        let _known = aqr_formula::store::known_mapped(&mapped);
        touch(&mapped);
        assert!(rss(&mapped) >= d.len() / 2);
        assert!(compress(&mapped, &opt) == plain);
        assert!(rss(&mapped) < d.len() / 8);
        // and as it is checksummed (as the archive is encoded: of all of it)
        touch(&mapped);
        assert_eq!(aqr::checksum_through(&mapped), aqr::checksum(&d));
        assert!(rss(&mapped) < d.len() / 8);
    }

    /// The chunks of a WIM file mapped are let go of once split, and once the data of each
    /// application is made of them again.
    #[cfg(target_os = "linux")]
    #[test]
    fn wim_chunks_let_go() {
        let d =
            std::fs::read(format!("{}/../../codecs/aqr-wim/tests/data/lzx.wim", env!("CARGO_MANIFEST_DIR"))).unwrap();
        let (mapped, rss) = mapped(&d);
        let _known = aqr_formula::store::known_mapped(&mapped);
        touch(&mapped);
        assert!(rss(&mapped) >= d.len() / 2);
        let f = wim(&mapped, 2).unwrap();
        // (the pages of the headers and of what is between the resources are left)
        let left = rss(&mapped);
        assert!(left < d.len() / 2, "{left} of {}", d.len());
        for s in f.streams.iter().filter(|s| s.data.is_lazy()) {
            drop(s.data.made());
        }
        assert!(rss(&mapped) <= left);
    }

    /// Forgetting data the encoder has not compressed (an image found smaller as its formula)
    /// reads none of it: not every page of an input mapped made resident again to hash it.
    #[cfg(target_os = "linux")]
    #[test]
    fn forget_reads_nothing() {
        let d = vec![7; 1 << 20];
        let (mapped, rss) = mapped(&d);
        let cx = Cx::new(&mapped, &Options::default());
        let before = rss(&mapped);
        cx.enc.forget(&mapped);
        assert_eq!(rss(&mapped), before);
    }

    /// An archive mapped from its file, decoded with its blocks let go of as they are read
    /// ([`aqr::Archive::let_go_read`]): its pages not resident once its file is written, the file
    /// the same.
    #[cfg(target_os = "linux")]
    #[test]
    fn archive_let_go_read() {
        // two streams of noise (stored as they are), a formula of them; and noise alone (a leaf
        // stored)
        let mut rng = aqr_common::rng::Rng::new(7);
        let (a, b) = (rng.bytes(512 << 10), rng.bytes(512 << 10));
        let mut f = Formula::default();
        f.root = Node::concat([f.push("a", a.clone()), f.push("b", b.clone())]);
        let whole = [a.clone(), b.clone()].concat();
        let raw = Formula::raw("raw", whole.clone());
        for f in [f, raw] {
            let c = aqr::encode(&f, None, aqr::Level::MID).unwrap();
            let (mapped, rss) = mapped(&c);
            let native = Native { threads: 2 };
            let runner = aqr_decompress::natives(&native);
            let mut archive = aqr::open(&mapped, &runner).unwrap();
            // SAFETY: a file mapped read only
            unsafe { archive.let_go_read() };
            let mut out = Vec::new();
            archive
                .write_file(2, &mut |p: &[u8]| {
                    out.extend_from_slice(p);
                    Ok(())
                })
                .unwrap();
            assert!(out == whole);
            assert!(rss(&mapped) < c.len() / 8, "{} of {}", rss(&mapped), c.len());
        }
    }

    /// The chunks of a WIM file mapped are let go of again once the contents of its data are
    /// made and its applications prevalidated (hashing the chunks they make).
    #[cfg(target_os = "linux")]
    #[test]
    fn wim_chunks_let_go_prevalidated() {
        let d =
            std::fs::read(format!("{}/../../codecs/aqr-wim/tests/data/lzx.wim", env!("CARGO_MANIFEST_DIR"))).unwrap();
        let (mapped, rss) = mapped(&d);
        let _known = aqr_formula::store::known_mapped(&mapped);
        let opt = Options { threads: 2, ..Options::default() };
        let mut f = wim(&mapped, 2).unwrap();
        let left = rss(&mapped);
        let mut cx = Cx::new(&mapped, &opt);
        cx.pre = Some(Arc::new(verify::Prevalidator::spawn(2, &[&mapped])));
        touch(&mapped);
        assert!(rss(&mapped) > left);
        nested(&mut f, &mapped, &opt, 0, &cx);
        assert!(cx.parts_checked.get());
        assert!(rss(&mapped) <= left, "{} > {left}", rss(&mapped));
    }

    /// The files of an image archived alone are sealed into the sink as they are made: the
    /// archive the same whatever the threads, and giving the image back.
    #[test]
    fn images_sealed() {
        let fixture = |p: &str| std::fs::read(format!("{}/../../{p}", env!("CARGO_MANIFEST_DIR"))).unwrap();
        let mut d = zstd::stream::decode_all(&fixture("codecs/aqr-iso/tests/data/udf201.iso.zst")[..]).unwrap();
        let jpeg = fixture("codecs/aqr-jpeg/tests/data/gradient.jpg");
        let files = aqr_disk::files(&d, bundle::FILE_SYSTEMS);
        let at = files.iter().find(|f| f.name == b"bin/data.bin").unwrap().extents[0].start;
        d[at..at + jpeg.len()].copy_from_slice(&jpeg);
        let archived = |threads| compress_formula(&d, &Options { threads, shared_modules: true, ..Options::default() });
        let (c, f) = archived(1);
        assert!(f.streams.iter().any(|s| s.data.sunk_in().is_some()));
        assert!(f.applies().iter().any(|a| a.module == aqr_decompress::JPEG));
        assert!(aqr_decompress::decompress(&c, 1).unwrap() == d);
        assert!(archived(4).0 == c);
    }

    /// An image in data whose formula is compared with the data ([`smaller`]), a gzip member: its
    /// files are not sealed (the comparison would read the streams sunk), and the archive gives
    /// the data back.
    #[test]
    fn images_compared_not_sealed() {
        let fixture = |p: &str| std::fs::read(format!("{}/../../{p}", env!("CARGO_MANIFEST_DIR"))).unwrap();
        let mut iso = zstd::stream::decode_all(&fixture("codecs/aqr-iso/tests/data/udf201.iso.zst")[..]).unwrap();
        let jpeg = fixture("codecs/aqr-jpeg/tests/data/gradient.jpg");
        let files = aqr_disk::files(&iso, bundle::FILE_SYSTEMS);
        let at = files.iter().find(|f| f.name == b"bin/data.bin").unwrap().extents[0].start;
        iso[at..at + jpeg.len()].copy_from_slice(&jpeg);
        let mut z = flate2::GzBuilder::new().filename("udf.iso").write(Vec::new(), flate2::Compression::new(6));
        std::io::Write::write_all(&mut z, &iso).unwrap();
        let d = z.finish().unwrap();
        let (c, f) = compress_formula(&d, &Options { threads: 2, shared_modules: true, ..Options::default() });
        assert!(f.streams.iter().all(|s| s.data.sunk_in().is_none()));
        assert!(f.applies().iter().any(|a| a.module == aqr_decompress::JPEG));
        assert!(aqr_decompress::decompress(&c, 1).unwrap() == d);
    }

    /// An image in a tar archive, the image's files sealed: where its formula passes the bound of
    /// the description, it is kept raw, its bytes as they are rather than its formula evaluated
    /// (which would read the streams sunk).
    #[test]
    fn sealed_over_the_bound_kept_raw() {
        let tar = tar_of;
        let fixture = |p: &str| std::fs::read(format!("{}/../../{p}", env!("CARGO_MANIFEST_DIR"))).unwrap();
        let mut iso = zstd::stream::decode_all(&fixture("codecs/aqr-iso/tests/data/udf201.iso.zst")[..]).unwrap();
        let jpeg = fixture("codecs/aqr-jpeg/tests/data/gradient.jpg");
        let files = aqr_disk::files(&iso, bundle::FILE_SYSTEMS);
        let at = files.iter().find(|f| f.name == b"bin/data.bin").unwrap().extents[0].start;
        iso[at..at + jpeg.len()].copy_from_slice(&jpeg);
        let text = b"some text, ".repeat(1000);
        let d = tar(&[("udf.iso", &iso), ("c.txt", &text)]);
        assert_eq!(detect(&d), Some(Format::Tar));
        // (the JPEG file within the image's bound, 165 bytes, the image not within the tar
        // archive's, 196)
        let opt = Options { description: 180, threads: 2, shared_modules: true, ..Options::default() };
        let (c, f) = compress_formula(&d, &opt);
        assert!(f.applies().iter().all(|a| a.module != aqr_decompress::JPEG));
        assert!(aqr_decompress::decompress(&c, 1).unwrap() == d);
    }

    /// An image whose first file of no kind (stream 0) is taken by its content's formula, and
    /// dropped from the image's, is still taken as an image once its contents are in place: not
    /// compared with its data (which would compress all of it).
    #[test]
    fn images_not_compared() {
        let fixture = |p: &str| std::fs::read(format!("{}/../../{p}", env!("CARGO_MANIFEST_DIR"))).unwrap();
        let mut d = zstd::stream::decode_all(&fixture("codecs/aqr-iso/tests/data/udf201.iso.zst")[..]).unwrap();
        let jpeg = fixture("codecs/aqr-jpeg/tests/data/gradient.jpg");
        let files = aqr_disk::files(&d, bundle::FILE_SYSTEMS);
        let first = aqr_disk::pieces(&d, &files)
            .into_iter()
            .find(|p| p.file.is_some_and(|(i, _)| kind_by_name(&files[i].name) == Kind::Unknown))
            .unwrap();
        assert!(first.span.len() >= jpeg.len());
        d[first.span.start..][..jpeg.len()].copy_from_slice(&jpeg);
        COMPARED.with(|c| c.borrow_mut().clear());
        let f = recompress(&d, &Options { shared_modules: true, ..Options::default() });
        assert!(f.applies().iter().any(|a| a.module == aqr_decompress::JPEG));
        assert!(f.eval(&Native::default()).unwrap() == d);
        assert!(!COMPARED.with(|c| c.borrow().contains(&d.len())));
    }

    /// A CAB file's MSZIP folder is taken apart into its files, recompressed in turn: a JPEG file
    /// in it by aqr-jpeg.
    #[test]
    fn cabs_taken_apart() {
        let d = std::fs::read(format!("{}/../../codecs/aqr-mszip/tests/data/jpeg.cab", env!("CARGO_MANIFEST_DIR")))
            .unwrap();
        assert_eq!(detect(&d), Some(Format::Cab));
        // (the modules the archive's anyway)
        let f = recompress(&d, &Options { shared_modules: true, ..Options::default() });
        assert!(f.applies().iter().any(|a| a.module == aqr_decompress::MSZIP));
        assert!(f.applies().iter().any(|a| a.module == aqr_decompress::JPEG));
        assert!(f.eval(&Native::default()).unwrap() == d);
    }

    /// LZX streams (CAB's folders, CHM's sections) are taken from level 9 only.
    #[test]
    fn lzx_from_level_9() {
        let at = |level| lzx_on(&Options { level, ..Options::default() });
        assert!(!at(Level::MID) && !at(Level::HIGH) && at(Level::Of(9)));
        // (and so CHM files, and streams of LZFSE and LZVN blocks)
        for format in [Format::Chm, Format::Bvx] {
            let at = |level| format_on(format, &Options { level, ..Options::default() });
            assert!(!at(Level::MID) && !at(Level::HIGH) && at(Level::Of(9)), "{format:?}");
        }
    }

    /// Levels 0 and 1 take no format at all: not even the files made beforehand ([`prepare`])
    /// are made, as they would only be thrown away.
    #[test]
    fn nothing_taken_at_level_1() {
        let jpeg =
            std::fs::read(format!("{}/../../codecs/aqr-jpeg/tests/data/gradient.jpg", env!("CARGO_MANIFEST_DIR")))
                .unwrap();
        let mut f = Formula::default();
        f.root = Node::concat([f.push_content("raw", Kind::Unknown, jpeg)]);
        for (level, n) in [(Level::STORED, 0), (Level::Of(1), 0), (Level::LOW, 1), (Level::MID, 1)] {
            let opt = Options { level, ..Options::default() };
            let mut jobs = Vec::new();
            pure_contents(&f.root, &f, &opt, &mut jobs);
            assert_eq!(jobs.len(), n, "{level:?}");
            assert_eq!(format_on(Format::Jpeg, &opt), n > 0, "{level:?}");
        }
    }

    /// LZMA and xz streams, 7z archives and GIF files are taken from level 4 only (up to level 3
    /// kept as they are).
    #[test]
    fn heavy_from_level_4() {
        let fixture = |p: &str| std::fs::read(format!("{}/../../{p}", env!("CARGO_MANIFEST_DIR"))).unwrap();
        let text: Vec<u8> =
            (0..30000).flat_map(|i| format!("line {} of the text\n", i * 7 % 1000).into_bytes()).collect();
        let files = [
            ("lzma", aqr_lzma::greedy::alone(&text)),
            ("7z", aqr_7z::write::solid(&[("a.txt", &text)])),
            ("gif", fixture("codecs/aqr-gif/tests/data/im-art.gif")),
        ];
        for (name, d) in &files {
            for (level, taken) in [(Level::LOW, false), (Level::Of(3), false), (Level::Of(4), true), (Level::MID, true)]
            {
                let f = recompress(d, &Options { level, shared_modules: true, ..Options::default() });
                assert_eq!(!f.applies().is_empty(), taken, "{name} {level:?}");
                assert!(f.eval(&Native::default()).unwrap() == *d, "{name} {level:?}");
            }
        }
    }

    /// An ALZ archive is taken apart: its DEFLATE bodies by aqr-flate, its bzip2 bodies by
    /// aqr-bz2's module of ALZip's streams; found in other data too.
    #[test]
    fn alz_debug() {
        let dir = format!("{}/../../codecs/aqr-alz/tests/data", env!("CARGO_MANIFEST_DIR"));
        let d = std::fs::read(format!("{dir}/normal.alz")).unwrap();
        let opt = Options { shared_modules: true, ..Options::default() };
        let cx = Cx::new(&d, &opt);
        let f = alz(&d, &opt, 0, &cx);
        eprintln!(
            "alz: {:?}",
            f.as_ref().map(|f| f.streams.iter().map(|s| (&s.class, s.data.len())).collect::<Vec<_>>())
        );
        let r = recompress_as(Format::Alz, &d, &opt, 0, &cx);
        eprintln!("as: {}", r.is_some());
    }

    #[test]
    fn installshield_cabinet_taken_apart() {
        // a data volume of chunks of zlib's DEFLATE, two files of text
        let text: Vec<u8> =
            (0..120_000u32).flat_map(|i| format!("setup {} file{} ", i % 977, i % 31).into_bytes()).collect();
        let mut d = b"ISc(\xb0\x04\x00\x02\0\0\0\0".to_vec();
        d.extend(0x200u32.to_le_bytes());
        d.extend([0; 4]);
        d.extend(0x200u32.to_le_bytes());
        d.resize(0x200, 0);
        for c in text[..100_000].chunks(32768).chain(text[100_000..].chunks(32768)) {
            let z = aqr_flate::zlib::compress(c, aqr_flate::zlib::Params::default()).unwrap();
            d.extend((z.len() as u16).to_le_bytes());
            d.extend(z);
        }
        assert_eq!(detect(&d), Some(Format::Isc));
        let opt = Options { shared_modules: true, level: Level::Of(9), ..Options::default() };
        let f = recompress(&d, &opt);
        assert!(f.applies().iter().any(|a| a.module == aqr_decompress::FLATE));
        assert!(f.eval(&Native::default()).unwrap() == d);
        // (known at the start of data only: not found in other data)
        assert_ne!(detect([&b"some data before "[..], &d].concat().as_slice()), Some(Format::Isc));
    }

    #[test]
    fn alz_taken_apart() {
        let dir = format!("{}/../../codecs/aqr-alz/tests/data", env!("CARGO_MANIFEST_DIR"));
        for (name, module) in [("normal", aqr_decompress::FLATE), ("bzip2", aqr_decompress::BZ2_ALZ)] {
            let d = std::fs::read(format!("{dir}/{name}.alz")).unwrap();
            assert_eq!(detect(&d), Some(Format::Alz));
            // (ALZip's DEFLATE is zlib's, which the adaptive profile emulates, from level 7)
            let opt = Options { shared_modules: true, level: Level::Of(9), ..Options::default() };
            let f = recompress(&d, &opt);
            assert!(f.applies().iter().any(|a| a.module == module), "{name}");
            assert!(f.eval(&Native::default()).unwrap() == d, "{name}");
            let x = [&b"some data before "[..], &d, b" and after"].concat();
            assert_eq!(detect::starts(&x[17..]), Some(Format::Alz));
            assert_eq!(extent(Format::Alz, &x[17..]), Some(d.len()));
        }
    }

    /// An Aqueous archive is taken apart: its module block the outer archive's own block of the
    /// module, its resource block one of the data as it is, and (at level 0) the data of its
    /// uncompressed blocks contents of their own; the rest as it is, damaged or not.
    #[test]
    fn aqr_taken_apart() {
        let dir = format!("{}/../../codecs/aqr-flac/tests/data", env!("CARGO_MANIFEST_DIR"));
        let flac = std::fs::read(format!("{dir}/s16_f5.flac")).unwrap();
        // (a file alone too small for the module to pay for itself: its formula taken anyway)
        let inner = compress(&flac, &Options { shared_modules: true, smaller: false, ..Options::default() });
        assert_eq!(detect(&inner), Some(Format::Aqr));
        let f = aqueous::archive(&inner).unwrap();
        let mut module_bytes = false;
        f.root.visit(&mut |n| module_bytes |= matches!(n, Node::ModuleBytes(m, _) if *m == aqr_decompress::FLAC));
        assert!(module_bytes);
        assert!(f.eval(&Native::default()).unwrap() == inner);
        // an archive of it, which has the module's block once
        let opt = Options { shared_modules: true, ..Options::default() };
        assert!(aqueous::is(&recompress(&inner, &opt)));
        let c = compress(&[&inner[..], &flac].concat(), &Options::default());
        assert!(aqr_decompress::decompress(&c, 1).unwrap() == [&inner[..], &flac].concat());
        // at level 0, the file in an uncompressed block: a content
        let stored = compress(&flac, &Options { level: Level::STORED, ..Options::default() });
        let f = aqueous::archive(&stored).unwrap();
        assert!(f.streams.iter().any(|s| s.content && s.data[..] == flac[..]));
        assert!(f.eval(&Native::default()).unwrap() == stored);
        // cut short, it is still given back
        for n in [7, 100, inner.len() / 2] {
            if let Some(f) = aqueous::archive(&inner[..n]) {
                assert!(f.eval(&Native::default()).unwrap() == inner[..n], "{n}");
            }
        }
        assert!(aqueous::archive(&flac).is_none());
    }

    /// An LHA archive is taken apart, its streams by aqr-lha's module, its stored files their
    /// contents; at the start of data only (its headers have no signature to look for). (The
    /// fixtures are too small for the module to pay for itself: the formula is kept all the
    /// same.)
    #[test]
    fn lha_taken_apart() {
        let dir = format!("{}/../../codecs/aqr-lha/tests/data", env!("CARGO_MANIFEST_DIR"));
        for name in ["unix_h2_lh7", "dos255e_lh0", "unix_lh6_long"] {
            let d = std::fs::read(format!("{dir}/{name}.lzh")).unwrap();
            assert_eq!(detect(&d), Some(Format::Lha), "{name}");
            let opt = Options { shared_modules: true, smaller: false, ..Options::default() };
            let f = recompress(&d, &opt);
            let module = f.applies().iter().any(|a| a.module == aqr_decompress::LHA);
            assert_eq!(module, name != "dos255e_lh0", "{name}");
            assert!(f.eval(&Native::default()).unwrap() == d, "{name}");
            let x = [&b"some data before "[..], &d].concat();
            assert_eq!(detect::starts(&x[17..]), None, "{name}");
        }
    }

    /// A RAR archive (RAR5 or RAR 3.x) is taken apart, its streams by aqr-rar's modules, its stored
    /// files their contents; found in other data too (after a self-extracting program).
    #[test]
    fn rar_taken_apart() {
        let dir = format!("{}/../../codecs/aqr-rar/tests/data", env!("CARGO_MANIFEST_DIR"));
        for name in ["r5_solid", "r4_solid", "r4_ppmd", "r5_stored"] {
            let d = std::fs::read(format!("{dir}/{name}.rar")).unwrap();
            assert_eq!(detect(&d), Some(Format::Rar), "{name}");
            // (PPMd from level 9 only)
            for level in [Level::MID, Level::Of(9)] {
                let opt = Options { shared_modules: true, smaller: false, level, ..Options::default() };
                let f = recompress(&d, &opt);
                let module =
                    f.applies().iter().any(|a| a.module == aqr_decompress::RAR5 || a.module == aqr_decompress::RAR3);
                let ppmd = name == "r4_ppmd";
                assert_eq!(module, name != "r5_stored" && (!ppmd || ppmd_on(&opt)), "{name} {level:?}");
                assert!(f.eval(&Native::default()).unwrap() == d, "{name}");
            }
            let x = [&b"some data before "[..], &d].concat();
            assert_eq!(detect::starts(&x[17..]), Some(Format::Rar), "{name}");
        }
    }

    /// An EGG archive is taken apart: its DEFLATE blocks by aqr-flate, its LZMA blocks by
    /// aqr-lzma; found in other data too.
    #[test]
    fn egg_taken_apart() {
        use std::io::Write;
        // (DEFLATE of zlib's, of words enough for its module to pay)
        let mut x = 1u32;
        let text: Vec<u8> = (0..40_000)
            .flat_map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                format!("w{} ", (x % 1000) * (x % 1000) / 1000).into_bytes()
            })
            .collect();
        let mut z = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::new(6));
        z.write_all(&text).unwrap();
        let n = text.len() as u32;
        let deflated = aqr_egg::egg::write(false, &[("a.txt", aqr_egg::egg::DEFLATE, &z.finish().unwrap(), n)]);
        // (an LZMA block: 4 bytes, the properties and the stream of a `.lzma` file)
        let alone = aqr_lzma::greedy::alone(&text);
        let body = [&[0; 4], &alone[..5], &alone[13..]].concat();
        let lzma = aqr_egg::egg::write(false, &[("a.txt", aqr_egg::egg::LZMA, &body, n)]);
        for (name, d, module) in [("deflate", deflated, aqr_decompress::FLATE), ("lzma", lzma, aqr_decompress::LZMA)] {
            assert_eq!(detect(&d), Some(Format::Egg));
            let opt = Options { shared_modules: true, level: Level::Of(9), ..Options::default() };
            let f = recompress(&d, &opt);
            assert!(f.applies().iter().any(|a| a.module == module), "{name}");
            assert!(f.eval(&Native::default()).unwrap() == d, "{name}");
            let x = [&b"some data before "[..], &d, b" and after"].concat();
            assert_eq!(detect::starts(&x[17..]), Some(Format::Egg));
            assert_eq!(extent(Format::Egg, &x[17..]), Some(d.len()));
        }
    }

    /// The contents of an application of several outputs have applications of several outputs
    /// of their own (a PDF file's streams, in an ALZ archive's DEFLATE bodies): those come before
    /// it, whose inputs take their outputs.
    #[test]
    fn shared_within_shared() {
        use std::io::Write;
        let deflate = |d: &[u8]| {
            let mut z = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::new(6));
            z.write_all(d).unwrap();
            z.finish().unwrap()
        };
        let mut x = 1u32;
        let text: Vec<u8> = (0..40_000)
            .flat_map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                format!("w{} ", (x % 1000) * (x % 1000) / 1000).into_bytes()
            })
            .collect();
        // a PDF file of three streams of zlib's, which aqr-pdf takes by one application of
        // several outputs
        let mut pdf = b"%PDF-1.4\n".to_vec();
        for (i, data) in text.chunks(text.len() / 3 + 1).enumerate() {
            let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::new(6));
            z.write_all(data).unwrap();
            let z = z.finish().unwrap();
            write!(pdf, "{} 0 obj\n<< /Length {} /Filter /FlateDecode >>\nstream\n", i + 1, z.len()).unwrap();
            pdf.extend_from_slice(&z);
            pdf.extend_from_slice(b"\nendstream\nendobj\n");
        }
        pdf.extend_from_slice(b"trailer\n<< >>\n%%EOF\n");
        let text = &text[..20_000];
        let (a, b) = (deflate(&pdf), deflate(text));
        // (local headers of sizes of 4 bytes, of DEFLATE)
        let mut d = b"ALZ\x01\x0a\0\0\0".to_vec();
        for (name, body, size) in [(&b"a.pdf"[..], &a, pdf.len()), (b"b.txt", &b, text.len())] {
            d.extend_from_slice(b"BLZ\x01\x05\x00\x20\0\0\0\0\x40\x00\x02\x00\0\0\0\0");
            d.extend_from_slice(&(body.len() as u32).to_le_bytes());
            d.extend_from_slice(&(size as u32).to_le_bytes());
            d.extend_from_slice(name);
            d.extend_from_slice(body);
        }
        d.extend_from_slice(b"CLZ\x02");
        let opt = Options { shared_modules: true, level: Level::Of(9), ..Options::default() };
        let f = recompress(&d, &opt);
        assert!(f.shared.len() >= 2, "{}", f.shared.len());
        f.check_outputs().unwrap();
        assert!(f.eval(&Native::default()).unwrap() == d);
    }

    /// The contents of pure formats are made beforehand, each once: those of a ZIP archive's
    /// entries are not made again where the first entry's own contents are looked for.
    #[test]
    fn contents_made_once() {
        let fixture = |p: &str| std::fs::read(format!("{}/../../{p}", env!("CARGO_MANIFEST_DIR"))).unwrap();
        let files = [
            fixture("codecs/aqr-jpeg/tests/data/gradient.jpg"),
            fixture("codecs/aqr-jpeg/tests/data/prog_script_recode.jpg"),
        ];
        let (mut d, mut cd) = (Vec::new(), Vec::new());
        for (i, data) in files.iter().enumerate() {
            let mut h = vec![20, 0, 0, 0, 0, 0, 0, 0, 0, 0];
            h.extend_from_slice(&aqr_common::crc32::hash(data).to_le_bytes());
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
        // (the modules the archive's anyway)
        let f = recompress(&d, &Options { threads: 1, shared_modules: true, ..Options::default() });
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
            std::io::Write::write_fmt(&mut t, format_args!("{} {} ", words[(x >> 61) as usize], (x >> 40) % 1000))
                .unwrap();
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

    /// With [`Options::mirrors`], every block whose damage would lose more than a file has a
    /// mirror block, an archive of a single file and of a tree alike, and gives its files back
    /// (with a block damaged and restored too).
    #[test]
    fn mirrors() {
        let mut z = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::new(6));
        std::io::Write::write_all(&mut z, &unruled(100_000)).unwrap();
        let gz = z.finish().unwrap();
        // (recompressed however small: a module block and a formula block)
        let opt = Options { threads: 2, mirrors: true, smaller: false, ..Options::default() };
        let checked = |c: &[u8]| {
            let m = aqr::mirror::check(c);
            assert!(m.walked && m.broken == 0 && m.near(false) == 0, "{m:?}");
            assert!(m.targets.len() >= 2 && m.unmirrored() == 0);
            assert_eq!(m.differing(c).count(), 0);
            m
        };
        let single = compress(&gz, &opt);
        assert!(single.len() > compress(&gz, &Options { mirrors: false, ..opt.clone() }).len());
        checked(&single);
        assert_eq!(aqr_decompress::decompress(&single, 2).unwrap(), gz);

        let text = unruled(50_000);
        let tree = aqr::Tree {
            entries: ["a.gz", "b.txt"]
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
        let inputs = [Input { name: b"a.gz", data: &gz }, Input { name: b"b.txt", data: &text }];
        let a = compress_tree(&tree, &inputs, &opt);
        assert_eq!(a.report.len, a.archive.len());
        assert!(a.report.mirrors > 0);
        let m = checked(&a.archive);
        let files = |c: &[u8]| match aqr_decompress::decompress_value(c, 2).unwrap() {
            aqr::Value::Tree(_, files) => files,
            aqr::Value::File(_) => panic!("a tree"),
        };
        assert_eq!(files(&a.archive), [gz.clone(), text.clone()]);
        let mut d = a.archive.clone();
        let meta = m.mirrors.last().unwrap();
        d[meta.target_end() - 1] ^= 0x10;
        assert!(aqr_decompress::decompress_value(&d, 2).is_err());
        let cd = aqr::mirror::check(&d);
        assert_eq!(aqr::mirror::repair(&d, &cd, false, &[]).unwrap().bytes, a.archive);
        // the metadata block's mirror block before the files (the archive is small: all of them)
        let meta = m.targets.last().unwrap().0;
        assert!(m.mirrors.iter().any(|f| f.target == meta && f.at < meta && f.far(false)));
        let d = &a.archive[..meta + 1];
        let r = aqr::mirror::repair(d, &aqr::mirror::check(d), false, &[]).unwrap();
        assert!(r.restored == 1 && r.footer && r.bytes.len() < a.archive.len());
        assert_eq!(files(&r.bytes), [gz.clone(), text.clone()]);
    }

    /// Of a text coded once for two, the formula kept is not evaluated beforehand again; nor is
    /// the first where the text is large.
    #[test]
    fn once_evaluated() {
        let (small, large) = (vec![b'a'; 1000], vec![b'a'; Once::LARGE + 1]);
        let key = |d: &[u8]| (d.len(), xxhash_rust::xxh3::xxh3_128(d));
        let once = Once::of([key(&small), key(&large)]);
        let (small2, large2) = (small.clone(), large.clone());
        for d in [&small, &small2, &large, &large2] {
            once.code(d, || Some(Formula::default()));
        }
        assert!(once.evaluated(&small) && !once.evaluated(&small2));
        assert!(!once.evaluated(&large) && !once.evaluated(&large2));
    }

    /// A stored ZIP archive of `files` (name, data).
    fn stored_zip(files: &[(&str, &[u8])]) -> Vec<u8> {
        let (mut d, mut cd) = (Vec::new(), Vec::new());
        for &(name, data) in files {
            let mut h = vec![20, 0, 0, 0, 0, 0, 0, 0, 0, 0];
            h.extend_from_slice(&aqr_common::crc32::hash(data).to_le_bytes());
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
            out.extend_from_slice(&aqr_common::crc32::hash(&out[start..]).to_be_bytes());
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
        let f = recompress(&d, &Options { threads: 1, shared_modules: true, ..Options::default() });
        assert_eq!(f.shared.len(), 1);
        assert_eq!(f.shared[0].sizes.len(), 3);
        assert_eq!(PURE.with(Cell::get), 1);
        assert!(f.eval(&Native::default()).unwrap() == d);
    }

    /// The sets planned are made where the formula of the contents is not the one the
    /// prevalidator evaluates (a copy of it, sharing its bytes).
    #[test]
    fn sets_of_copies() {
        let files = [
            variant(200, 150, None),
            variant(200, 150, Some((20, 30, 40, 50))),
            variant(200, 150, Some((90, 10, 30, 90))),
        ];
        let d = stored_zip(&[("a.png", &files[0]), ("b.png", &files[1]), ("c.png", &files[2])]);
        let (c, f) = compress_formula(&d, &Options { threads: 2, shared_modules: true, ..Options::default() });
        assert_eq!(f.shared.len(), 1);
        assert!(aqr_decompress::decompress(&c, 1).unwrap() == d);
    }

    /// Among files made apart (an image's) of which sets are planned, those not in a set still
    /// take the bytes of their streams as they are made ([`make_later`]); the members keep
    /// theirs, which the sets are made of.
    #[test]
    fn taken_beside_sets() {
        let jpeg =
            std::fs::read(format!("{}/../../codecs/aqr-jpeg/tests/data/gradient.jpg", env!("CARGO_MANIFEST_DIR")))
                .unwrap();
        let named = [
            (&b"a.png"[..], variant(200, 150, None)),
            (b"b.png", variant(200, 150, Some((20, 30, 40, 50)))),
            (b"c.png", variant(200, 150, Some((90, 10, 30, 90)))),
            (b"j.jpg", jpeg),
        ];
        let (mut d, mut files, mut pieces) = (Vec::new(), Vec::new(), Vec::new());
        for (i, (name, data)) in named.iter().enumerate() {
            let span = d.len()..d.len() + data.len();
            d.extend_from_slice(data);
            files.push(aqr_disk::File { name: name.to_vec(), extents: vec![span.clone()], rank: 0, compressed: None });
            pieces.push(aqr_disk::Piece { span, file: Some((i, 0)) });
        }
        let opt =
            aqr_disk::formula::Options { file_systems: bundle::FILE_SYSTEMS, kind: kind_by_name, ..Default::default() };
        let mut f = aqr_disk::formula::build(&d, &files, &pieces, &opt);
        let opt = Options { threads: 2, shared_modules: true, ..Options::default() };
        let cx = Cx::new(&d, &opt);
        let (_, plan) = nested(&mut f, &d, &opt, 0, &cx).unwrap();
        assert!(!plan.is_empty());
        let of = |class: &str| f.streams.iter().filter(|s| s.class == class).map(|s| s.data.len()).collect::<Vec<_>>();
        assert!(of("disk/png").len() == 3 && of("disk/png").iter().all(|&n| n > 0));
        assert_eq!(of("disk/jpeg"), [0]);
    }

    /// A FAT volume's JPEG file in pieces (aqr-fat's fixture) is recompressed by aqr-jpeg as one
    /// content, through the archive; the archive the same whatever the threads.
    #[test]
    fn fat_files_in_pieces() {
        let z = std::fs::read(format!("{}/../../codecs/aqr-fat/tests/data/fat32.img.zst", env!("CARGO_MANIFEST_DIR")))
            .unwrap();
        let d = zstd::stream::decode_all(&z[..]).unwrap();
        assert_eq!(detect(&d), Some(Format::Disk));
        let files = aqr_disk::files(&d, bundle::FILE_SYSTEMS);
        assert!(files.iter().find(|f| f.name == b"photo.jpg").unwrap().extents.len() > 1);
        let archived = |threads| compress_formula(&d, &Options { threads, shared_modules: true, ..Options::default() });
        let (c, f) = archived(2);
        assert!(f.applies().iter().any(|a| a.module == aqr_decompress::JPEG));
        assert!(!f.shared.is_empty());
        assert!(aqr_decompress::decompress(&c, 1).unwrap() == d);
        assert!(archived(1).0 == c);
    }

    /// An NTFS volume's files (aqr-ntfs's fixture): its JPEG file recompressed by aqr-jpeg, its
    /// files in pieces split, the archive giving it back.
    #[test]
    fn ntfs_volumes_taken_apart() {
        let z = std::fs::read(format!("{}/../../codecs/aqr-ntfs/tests/data/ntfs.img.zst", env!("CARGO_MANIFEST_DIR")))
            .unwrap();
        let d = zstd::stream::decode_all(&z[..]).unwrap();
        assert_eq!(detect(&d), Some(Format::Disk));
        let (c, f) = compress_formula(&d, &Options { threads: 2, shared_modules: true, ..Options::default() });
        assert!(f.applies().iter().any(|a| a.module == aqr_decompress::JPEG));
        assert!(!f.shared.is_empty());
        assert!(aqr_decompress::decompress(&c, 1).unwrap() == d);
    }

    /// UDIF images (aqr-disk's fixtures, of an HFS+ volume): of zlib's, bzip2's or LZFSE's chunks, its own
    /// formula (the disk a content, taken apart in turn: its JPEG file by aqr-jpeg; zlib's chunks
    /// by zlib's emulation), of raw ones a container of the disk; all given back.
    #[test]
    fn dmg_taken_apart() {
        let fixture = |name: &str| {
            let p = format!("{}/../../codecs/aqr-disk/tests/data/{name}.zst", env!("CARGO_MANIFEST_DIR"));
            zstd::stream::decode_all(&std::fs::read(p).unwrap()[..]).unwrap()
        };
        let opt = Options { threads: 2, shared_modules: true, ..Options::default() };
        let d = fixture("dmg-udzo.dmg");
        assert_eq!(detect(&d), Some(Format::Dmg));
        // (hdiutil's zlib emulated: its records next to nothing)
        let f = chunked::dmg(&d, &opt, &Cx::new(&d, &opt)).unwrap();
        let records: usize = f.streams.iter().filter(|s| s.class == "flate/record").map(|s| s.data.len()).sum();
        let chunks = aqr_disk::dmg::parse(&d).unwrap().chunks;
        let zlib: usize = chunks.iter().filter(|c| c.how == aqr_disk::dmg::How::Zlib).map(|c| c.file.len()).sum();
        assert!(records * 50 < zlib, "records {records} of {zlib} B");
        let (c, f) = compress_formula(&d, &opt);
        assert!(f.applies().iter().any(|a| a.module == aqr_flate::formula::MODULE));
        assert!(f.applies().iter().any(|a| a.module == aqr_decompress::JPEG));
        assert!(aqr_decompress::decompress(&c, 1).unwrap() == d);
        let d = fixture("dmg-udbz.dmg");
        assert_eq!(detect(&d), Some(Format::Dmg));
        let (c, f) = compress_formula(&d, &opt);
        assert!(f.applies().iter().any(|a| a.module == aqr_bz2::formula::MODULE));
        assert!(f.applies().iter().any(|a| a.module == aqr_decompress::JPEG));
        assert!(aqr_decompress::decompress(&c, 1).unwrap() == d);
        let d = fixture("dmg-ulfo.dmg");
        assert_eq!(detect(&d), Some(Format::Dmg));
        let (c, f) = compress_formula(&d, &opt);
        assert!(f.applies().iter().any(|a| a.module == aqr_bvx::formula::MODULE));
        assert!(aqr_decompress::decompress(&c, 1).unwrap() == d);
        let d = fixture("dmg-udro.dmg");
        assert_eq!(detect(&d), Some(Format::Disk));
        let (c, f) = compress_formula(&d, &opt);
        assert!(f.applies().iter().any(|a| a.module == aqr_decompress::JPEG));
        assert!(aqr_decompress::decompress(&c, 1).unwrap() == d);
    }

    /// Compressed containers (aqr-disk's fixtures, of qemu-img: a qcow2 image of compressed clusters,
    /// raw DEFLATE of zlib's level 6 of a window of 4 KiB; a streamOptimized VMDK extent, zlib's
    /// level 6): their own formula, as a DMG's (the compressed chunks by zlib's emulation, the
    /// files taken apart: its JPEG file by aqr-jpeg), given back, and verified by their hashes
    /// without running a module.
    #[test]
    fn compressed_containers_taken_apart() {
        for name in ["disk-c.qcow2", "disk-s.vmdk", "disk.E01"] {
            let p = format!("{}/../../codecs/aqr-disk/tests/data/{name}.zst", env!("CARGO_MANIFEST_DIR"));
            let d = zstd::stream::decode_all(&std::fs::read(p).unwrap()[..]).unwrap();
            assert_eq!(detect(&d), Some(Format::Disk), "{name}");
            let opt = Options { threads: 2, shared_modules: true, ..Options::default() };
            let f = chunked::container(&d, &opt, &Cx::new(&d, &opt)).unwrap();
            let records: usize = f.streams.iter().filter(|s| s.class == "flate/record").map(|s| s.data.len()).sum();
            let chunks = aqr_disk::chunked::container(&d).unwrap().chunks;
            let compressed: usize = (chunks.iter())
                .filter(|c| matches!(c.how, aqr_disk::chunked::How::Deflate | aqr_disk::chunked::How::Zlib))
                .map(|c| c.file.len())
                .sum();
            // (an EWF image's chunks are of 32 KiB, most of zeros: each a record of some bytes)
            let most = if name.ends_with(".E01") { 15 } else { 50 };
            assert!(records * most < compressed, "{name}: records {records} of {compressed} B");
            let (c, f) = compress_formula(&d, &opt);
            assert!(f.applies().iter().any(|a| a.module == aqr_flate::formula::MODULE), "{name}");
            assert!(f.applies().iter().any(|a| a.module == aqr_decompress::JPEG), "{name}");
            assert!(aqr_decompress::decompress(&c, 1).unwrap() == d, "{name}");
            let opt = Options { smaller: false, ..opt };
            let mut cx = Cx::new(&d, &opt);
            cx.pre = Some(Arc::new(verify::Prevalidator::spawn(2, &[&d])));
            let f = recompress_at(&d, &opt, 0, Kind::Unknown, &cx);
            let memo = verify::Prevalidator::finish_shared(cx.pre.take().unwrap());
            let c = cx.enc.encode(&f, Some(aqr::checksum(&d))).unwrap();
            let roots = aqr::open(&c, &aqr::NoModules).unwrap().hashed(&memo, &aqr_decompress::identify, 2);
            let roots = roots.map_err(|e| e.to_string()).unwrap().expect("told");
            let whole = roots.iter().fold(aqr_formula::chash::Ch::EMPTY, |w, (h, _)| w.cat(*h));
            assert!(whole == aqr_formula::chash::Ch::of(&d), "{name}");
        }
    }

    /// An EWF image's segment files, each taken by itself (the second of raw chunks only): each
    /// a disk image, its chunks rebuilt.
    #[test]
    fn ewf_segments_taken_apart() {
        for name in ["disk-s.E01", "disk-s.E02", "disk-s.E03"] {
            let p = format!("{}/../../codecs/aqr-disk/tests/data/{name}.zst", env!("CARGO_MANIFEST_DIR"));
            let d = zstd::stream::decode_all(&std::fs::read(p).unwrap()[..]).unwrap();
            assert_eq!(detect(&d), Some(Format::Disk), "{name}");
            let opt = Options { threads: 2, shared_modules: true, ..Options::default() };
            assert!(chunked::container(&d, &opt, &Cx::new(&d, &opt)).is_some(), "{name}");
            let (c, _) = compress_formula(&d, &opt);
            assert!(c.len() * 4 < d.len(), "{name}: {} of {} B", c.len(), d.len());
            assert!(aqr_decompress::decompress(&c, 1).unwrap() == d, "{name}");
        }
    }

    /// A DMG's archive is verified by its hashes without running a module: its disk's Split into
    /// the chunks, of pieces that the chunks do not cut where they are cut, is told by what the
    /// formula recorded of it as it was made (the disk's bytes at hand then), and the disk's
    /// applications (its JPEG file's) by the prevalidation.
    #[test]
    fn dmg_verified_by_hashes() {
        let p = format!("{}/../../codecs/aqr-disk/tests/data/dmg-udzo.dmg.zst", env!("CARGO_MANIFEST_DIR"));
        let d = zstd::stream::decode_all(&std::fs::read(p).unwrap()[..]).unwrap();
        let opt = Options { threads: 2, smaller: false, ..Options::default() };
        let mut cx = Cx::new(&d, &opt);
        cx.pre = Some(Arc::new(verify::Prevalidator::spawn(2, &[&d])));
        let f = recompress_at(&d, &opt, 0, Kind::Unknown, &cx);
        let memo = verify::Prevalidator::finish_shared(cx.pre.take().unwrap());
        assert!(f.applies().iter().any(|a| a.module == aqr_decompress::JPEG));
        let c = cx.enc.encode(&f, Some(aqr::checksum(&d))).unwrap();
        let a = aqr::open(&c, &aqr::NoModules).unwrap();
        let roots = a.hashed(&memo, &aqr_decompress::identify, 2).map_err(|e| e.to_string());
        let roots = roots.unwrap().expect("told");
        let whole = roots.iter().fold(aqr_formula::chash::Ch::EMPTY, |w, (h, _)| w.cat(*h));
        assert!(whole == aqr_formula::chash::Ch::of(&d));
    }

    /// A UDIF image of zlib's chunks whose block tables are changed (chunks of other runs of the
    /// disk or the file, overlapping, of another kind): no formula, or one giving it back.
    #[test]
    fn dmg_damaged() {
        let p = format!("{}/../../codecs/aqr-disk/tests/data/dmg-udzo.dmg.zst", env!("CARGO_MANIFEST_DIR"));
        let d = zstd::stream::decode_all(&std::fs::read(p).unwrap()[..]).unwrap();
        let u = aqr_disk::dmg::parse(&d).unwrap();
        let opt = Options { threads: 2, ..Options::default() };
        let mut tried = 0;
        for p in u.xml.clone().step_by(5) {
            for v in [b'A', b'/', d[p] ^ 0x01] {
                let mut e = d.clone();
                e[p] = v;
                // (where the chunks read are others)
                if aqr_disk::dmg::parse(&e).is_none_or(|t| t.chunks == u.chunks) {
                    continue;
                }
                tried += 1;
                if let Some(f) = chunked::dmg(&e, &opt, &Cx::new(&e, &opt)) {
                    assert!(f.eval(&Native::default()).unwrap() == e, "byte {p} = {v:#x}");
                }
            }
        }
        assert!(tried > 10, "{tried}");
    }

    /// An APFS container's files (aqr-apfs's fixture, of a GPT): its JPEG file recompressed by
    /// aqr-jpeg, the image given back.
    #[test]
    fn apfs_containers_taken_apart() {
        let z = std::fs::read(format!("{}/../../codecs/aqr-apfs/tests/data/apfs.img.zst", env!("CARGO_MANIFEST_DIR")))
            .unwrap();
        let d = zstd::stream::decode_all(&z[..]).unwrap();
        assert_eq!(detect(&d), Some(Format::Disk));
        let (c, f) = compress_formula(&d, &Options { threads: 2, shared_modules: true, ..Options::default() });
        assert!(f.applies().iter().any(|a| a.module == aqr_decompress::JPEG));
        assert!(aqr_decompress::decompress(&c, 1).unwrap() == d);
    }

    /// An ext4 volume's files (aqr-ext's fixture): its JPEG file recompressed by aqr-jpeg, its
    /// files in pieces split, the archive giving it back.
    #[test]
    fn ext_volumes_taken_apart() {
        let z = std::fs::read(format!("{}/../../codecs/aqr-ext/tests/data/ext4.img.zst", env!("CARGO_MANIFEST_DIR")))
            .unwrap();
        let d = zstd::stream::decode_all(&z[..]).unwrap();
        assert_eq!(detect(&d), Some(Format::Disk));
        let (c, f) = compress_formula(&d, &Options { threads: 2, shared_modules: true, ..Options::default() });
        assert!(f.applies().iter().any(|a| a.module == aqr_decompress::JPEG));
        assert!(!f.shared.is_empty());
        assert!(aqr_decompress::decompress(&c, 1).unwrap() == d);
    }

    /// An NTFS volume's files Windows compressed by WOF's LZX and XPRESS (aqr-ntfs's fixture):
    /// their data contents, their chunks rebuilt by aqr-lzx's and aqr-xpress's modules, the
    /// volume given back.
    #[test]
    fn wof_taken_apart() {
        let z =
            std::fs::read(format!("{}/../../codecs/aqr-ntfs/tests/data/wof-small.vhd.zst", env!("CARGO_MANIFEST_DIR")))
                .unwrap();
        let d = zstd::stream::decode_all(&z[..]).unwrap();
        let (c, f) = compress_formula(&d, &Options { threads: 2, shared_modules: true, ..Options::default() });
        assert!(f.applies().iter().any(|a| a.module == aqr_decompress::LZX));
        assert!(f.applies().iter().any(|a| a.module == aqr_decompress::XPRESS));
        assert!(aqr_decompress::decompress(&c, 1).unwrap() == d);
    }

    /// A virtual disk in a container (aqr-disk's VHDX fixture, of a GPT of ext volumes): its files
    /// taken where the container holds them, its JPEG files recompressed by aqr-jpeg, the
    /// container given back.
    #[test]
    fn containers_taken_apart() {
        let z = std::fs::read(format!("{}/../../codecs/aqr-disk/tests/data/disk.vhdx.zst", env!("CARGO_MANIFEST_DIR")))
            .unwrap();
        let d = zstd::stream::decode_all(&z[..]).unwrap();
        assert_eq!(detect(&d), Some(Format::Disk));
        let (c, f) = compress_formula(&d, &Options { threads: 2, shared_modules: true, ..Options::default() });
        assert!(f.applies().iter().any(|a| a.module == aqr_decompress::JPEG));
        assert!(aqr_decompress::decompress(&c, 1).unwrap() == d);
    }

    /// A file of an image in pieces, split from its stream ([`aqr_formula::Of::Node`]), is one
    /// content: a JPEG file in three pieces out of order, about a text file, recompressed by
    /// aqr-jpeg, and the image rebuilt.
    #[test]
    #[allow(clippy::single_range_in_vec_init)]
    fn files_in_pieces_taken() {
        let jpeg =
            std::fs::read(format!("{}/../../codecs/aqr-jpeg/tests/data/gradient.jpg", env!("CARGO_MANIFEST_DIR")))
                .unwrap();
        let text = b"some text between the pieces, ".repeat(50);
        let (n, a, b) = (jpeg.len(), jpeg.len() / 3, jpeg.len() * 2 / 3);
        // its middle, the text, its end, its start
        let d = [&jpeg[a..b], &text, &jpeg[b..], &jpeg[..a]].concat();
        let (t, e, s) = (b - a, b - a + text.len(), b - a + text.len() + n - b);
        let files = [
            aqr_disk::File {
                name: b"j.jpg".to_vec(),
                extents: vec![s..s + a, 0..b - a, e..e + n - b],
                rank: 0,
                compressed: None,
            },
            aqr_disk::File { name: b"t.txt".to_vec(), extents: vec![t..e], rank: 0, compressed: None },
        ];
        let pieces = aqr_disk::pieces(&d, &files);
        let opt =
            aqr_disk::formula::Options { file_systems: bundle::FILE_SYSTEMS, kind: kind_by_name, ..Default::default() };
        let mut f = aqr_disk::formula::build(&d, &files, &pieces, &opt);
        assert_eq!(f.shared.len(), 1);
        let opt = Options { threads: 2, shared_modules: true, ..Options::default() };
        let cx = Cx::new(&d, &opt);
        let (children, plan) = nested(&mut f, &d, &opt, 0, &cx).unwrap();
        assert!(plan.is_empty());
        place_all(&mut f, children.into_iter().map(|c| c.1).collect(), Vec::new(), &opt);
        let jpegs = |n: &Node| matches!(n, Node::Apply(a) if a.module == aqr_decompress::JPEG);
        assert!(f.shared.iter().any(|s| matches!(&s.of, aqr_formula::Of::Node(n) if jpegs(n))));
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
        let deconstructed = |d: &[u8], smaller| {
            DECONSTRUCTED.with(|n| n.set(0));
            let f = recompress(d, &Options { smaller, ..Options::default() });
            (DECONSTRUCTED.with(Cell::get), f.applies().len())
        };
        // (named as compressed data, which is not scanned for files)
        let noise = aqr_common::rng::noise(300_000);
        assert_eq!(deconstructed(&gz("noise.mp4", &noise, 6), true), (0, 0));
        // DEFLATE at level 1 leaves much to the archive
        let text: Vec<u8> =
            (0..30000).flat_map(|i| format!("line {} of the text\n", i * 7 % 1000).into_bytes()).collect();
        // (kept however large it archives with its modules)
        let (n, applies) = deconstructed(&gz("text.txt", &text, 1), false);
        assert_eq!(n, 1);
        assert!(applies > 0);
    }

    /// A recompression whose formula has an application over `application_bound` is not used,
    /// and what fell back is told ([`fallbacks`]); under the default bound it is used.
    #[test]
    fn application_bound_falls_back() {
        let mut z = flate2::GzBuilder::new().filename("text.txt").write(Vec::new(), flate2::Compression::new(1));
        let text: Vec<u8> =
            (0..30000).flat_map(|i| format!("line {} of the text\n", i * 7 % 1000).into_bytes()).collect();
        std::io::Write::write_all(&mut z, &text).unwrap();
        let gz = z.finish().unwrap();
        let made = |application_bound| {
            let opt = Options { smaller: false, application_bound, ..Options::default() };
            let f = recompress(&gz, &opt);
            let back = f.eval(&Native::default());
            assert!(back.as_ref().is_ok_and(|b| *b == gz));
            f.applies().len()
        };
        let _seen = FALLBACKS_SEEN.lock().unwrap_or_else(|e| e.into_inner());
        let _ = fallbacks();
        assert!(made(None) > 0);
        assert!(fallbacks().is_empty());
        // (what fell back tells the exact minimal bound the application is declared with)
        let mut f = recompress(&gz, &Options { smaller: false, ..Options::default() });
        memory::declare(&mut f, &Native { threads: 1 }, &memory::Limits::default());
        let declared = f.applies().iter().map(|a| aqr_formula::minimal_bound(a.memory_limit, &a.task_limits)).max();
        let _ = fallbacks();
        // (the plan of an application is more than a byte)
        assert_eq!(made(Some(1)), 0);
        let told = fallbacks();
        assert!(!told.is_empty() && told.iter().all(|f| f.needed > 1 && !f.kept), "{told:?}");
        assert!(told.iter().any(|f| f.bytes == gz.len() as u64));
        assert_eq!(told.iter().map(|f| f.needed).max(), declared);
        assert_eq!(bound_text(256 << 20), "268435456 B = 256.00 MiB");
        assert_eq!(bound_text((256 << 20) + 1), "268435457 B = 256.01 MiB");
    }

    /// The members of a gzip stream after its first (as gzip, pigz's `--independent` and
    /// aqr-pack write a stream in parts) are deconstructed too, their data one stream with the
    /// first's; a member that is not one ends them, what follows it kept as it is.
    #[test]
    fn gzip_members_as_one() {
        let text: Vec<u8> =
            (0..60000).flat_map(|i| format!("line {} of the text\n", i * 7 % 1000).into_bytes()).collect();
        let member = |name: Option<&str>, d: &[u8]| {
            let b = flate2::GzBuilder::new();
            let b = if let Some(n) = name { b.filename(n) } else { b };
            let mut z = b.write(Vec::new(), flate2::Compression::new(6));
            std::io::Write::write_all(&mut z, d).unwrap();
            z.finish().unwrap()
        };
        let parts: Vec<&[u8]> = text.chunks(text.len() / 3 + 1).collect();
        let mut gz = member(Some("text.txt"), parts[0]);
        gz.extend(member(None, parts[1]));
        gz.extend(member(None, parts[2]));
        gz.extend(b"\x1f\x8b\x08 not a member");
        DECONSTRUCTED.with(|n| n.set(0));
        let f = recompress(&gz, &Options { smaller: false, ..Options::default() });
        assert_eq!(DECONSTRUCTED.with(Cell::get), 3);
        let back = f.eval(&Native::default());
        assert!(back.as_ref().is_ok_and(|b| *b == gz), "{:?}", back.map(|b| b.len()));
        // (one text of them all)
        let texts: Vec<usize> = f.streams.iter().filter(|s| s.kind == Kind::Text).map(|s| s.data.len()).collect();
        assert_eq!(texts.len(), 1, "{texts:?}");
        // (what follows, not a member, kept as it is)
        assert!(f.streams.iter().any(|s| s.data.windows(12).any(|w| w == b"not a member")));
    }

    /// Level 1 recompresses nothing; levels 2 and 3 keep DEFLATE streams as they are
    /// ([`inflated`]); from 7 on, they are always deconstructed (no probe), with the adaptive
    /// profile.
    #[test]
    fn levels() {
        let gz = |name: &str, d: &[u8], level| {
            let mut z = flate2::GzBuilder::new().filename(name).write(Vec::new(), flate2::Compression::new(level));
            std::io::Write::write_all(&mut z, d).unwrap();
            z.finish().unwrap()
        };
        let made = |d: &[u8], level| {
            DECONSTRUCTED.with(|n| n.set(0));
            // (sized without the module blocks, as among many files)
            let f = recompress(d, &Options { level, shared_modules: true, ..Options::default() });
            let profile = f
                .streams
                .iter()
                .find(|s| s.class == "flate/record")
                .map(|s| aqr_flate::record_profile(&s.data).unwrap());
            (DECONSTRUCTED.with(Cell::get), profile)
        };
        let noise = gz("noise.mp4", &aqr_common::rng::noise(300_000), 6);
        let text: Vec<u8> =
            (0..30000).flat_map(|i| format!("line {} of the text\n", i * 7 % 1000).into_bytes()).collect();
        // (DEFLATE at level 1 leaves much to the archive)
        let text = gz("text.txt", &text, 1);
        for level in
            [Level::Of(1), Level::LOW, Level::Of(3), Level::Of(4), Level::MID, Level::Of(7), Level::HIGH, Level::Of(9)]
        {
            let n = level.scale();
            // (noise turned down by the probe, or else after it is deconstructed)
            assert_eq!(made(&noise, level).0, (n >= 7) as usize, "{level:?}");
            let profile = match n {
                ..4 => None,
                4..7 => Some(aqr_flate::DEFAULT_PROFILE),
                _ => Some(aqr_flate::ADAPTIVE),
            };
            assert_eq!(made(&text, level), ((n >= 4) as usize, profile), "{level:?}");
        }
    }

    /// PDF files at the archive's level: the records of their DEFLATE streams of aqr-flate's
    /// profile as [`Options::flate`] tells, and from level 7 on with no time budget (every
    /// FlateDecode stream inflated, none in the token mode or raw).
    #[test]
    fn pdf_levels() {
        let mut rng = aqr_common::rng::Rng::new(1);
        let mut pdf = b"%PDF-1.7\n".to_vec();
        for i in 0..200 {
            let content: String = (0..200)
                .map(|_| format!("BT /F1 12 Tf {} {} Td (w{}) Tj ET\n", rng.below(600), rng.below(800), rng.below(50)))
                .collect();
            let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::new(6));
            std::io::Write::write_all(&mut z, content.as_bytes()).unwrap();
            let z = z.finish().unwrap();
            pdf.extend_from_slice(
                format!("{} 0 obj\n<< /Length {} /Filter /FlateDecode >>\nstream\n", i + 1, z.len()).as_bytes(),
            );
            pdf.extend_from_slice(&z);
            pdf.extend_from_slice(b"\nendstream\nendobj\n");
        }
        pdf.extend_from_slice(b"trailer\n<< /Size 201 >>\n%%EOF\n");
        assert_eq!(detect(&pdf), Some(Format::Pdf));
        for level in [Level::MID, Level::HIGH] {
            let f = recompress(&pdf, &Options { level, shared_modules: true, ..Options::default() });
            let stream = |class: &str| &f.streams.iter().find(|s| s.class == class).expect(class).data;
            // (none at mid, where the budget leaves them all to the token mode)
            let profile = aqr_flate::record_profile(stream("pdf/meta")).ok();
            let want = Options { level, ..Options::default() }.flate().0;
            assert!(profile.is_none_or(|p| p == want) && (level == Level::MID || profile.is_some()), "{level:?}");
            let raw_or_tok = !stream("pdf/raw").is_empty() || !stream("pdf/tok-symbols").is_empty();
            assert_eq!(raw_or_tok, level == Level::MID, "{level:?}");
        }
    }

    /// A ustar archive of `files`.
    fn tar_of(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut t = Vec::new();
        for (name, d) in files {
            let mut h = [0u8; 512];
            h[..name.len()].copy_from_slice(name.as_bytes());
            h[100..107].copy_from_slice(b"0000644");
            h[108..115].copy_from_slice(b"0000000");
            h[116..123].copy_from_slice(b"0000000");
            h[124..135].copy_from_slice(format!("{:011o}", d.len()).as_bytes());
            h[136..147].copy_from_slice(b"00000000000");
            h[156] = b'0';
            h[257..265].copy_from_slice(b"ustar\x0000");
            h[148..156].fill(b' ');
            let sum: u32 = h.iter().map(|&b| b as u32).sum();
            h[148..155].copy_from_slice(format!("{sum:06o}\0").as_bytes());
            t.extend_from_slice(&h);
            t.extend_from_slice(d);
            t.resize(t.len().next_multiple_of(512), 0);
        }
        t.resize(t.len() + 1024, 0);
        t
    }

    /// The text members of a tar archive are coded with one dictionary, so that the same
    /// words code alike in files apart (each with a dictionary of its own, the zstd of the
    /// coded files finds nothing across them).
    #[test]
    fn tar_members_share_words() {
        let words: Vec<String> = (0..300)
            .map(|i| {
                let n = i * 7919 + 13;
                format!(
                    "{}{}{}",
                    ["ab", "ka", "to", "mi", "re"][n % 5],
                    ["lon", "pra", "esh", "ud"][n / 5 % 4],
                    ["ing", "ed", "s", "ly", "er"][n / 20 % 5]
                )
            })
            .collect();
        let text = |seed: u64| {
            let mut x = seed;
            let mut t = Vec::new();
            while t.len() < 300_000 {
                x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                t.extend_from_slice(words[(x >> 40) as usize % words.len()].as_bytes());
                t.push(b' ');
            }
            t
        };
        let texts: Vec<Vec<u8>> = (1..=4).map(text).collect();
        let names: Vec<String> = (0..4).map(|i| format!("t{i}.dat")).collect();
        let files: Vec<(&str, &[u8])> = names.iter().map(|n| n.as_str()).zip(texts.iter().map(|t| &t[..])).collect();
        let d = tar_of(&files);
        let opt = Options { threads: 2, ..Options::default() };
        let (c, f) = compress_formula(&d, &opt);
        let coded: Vec<_> = f.applies().into_iter().filter(|a| a.module == aqr_decompress::WRT).collect();
        assert!(coded.len() >= 2, "{} wrt applications", coded.len());
        assert!(coded.iter().all(|a| a.resources == coded[0].resources));
        assert!(aqr_decompress::decompress(&c, 1).unwrap() == d);
    }

    /// A file of text that aqr-wrt does not take is kept raw as text, archived as text is
    /// (dense from level 8 on).
    #[test]
    fn text_kept_raw() {
        // (of no words, which aqr-wrt would take)
        let text: Vec<u8> = (0..40000).flat_map(|i| format!("{i}: {}\n", i * 7).into_bytes()).collect();
        assert!(text.len() >= aqr_wrt::formula::MIN_LEN);
        assert_eq!(detect(&text), Some(Format::Text));
        let f = recompress(&text, &Options::default());
        assert!(f.applies().is_empty());
        assert_eq!(f.streams[0].kind, Kind::Text);

        // a file alone of a kind by its name, as one in an archive of many: text too short for
        // aqr-wrt by a name of text, compressed data stored
        let opt = Options { verify: false, ..Options::default() };
        let short = &text[..100_000];
        assert_eq!(detect(short), None);
        for (name, kind) in [(&b"short.txt"[..], Kind::Text), (b"short.log", Kind::Text), (b"short", Kind::Unknown)] {
            let (c, f) = compress_formula_as(short, kind_by_name(name), &opt);
            assert_eq!(f.streams[0].kind, kind, "{}", String::from_utf8_lossy(name));
            assert_eq!(
                aqr::decode(&c, &aqr_decompress::natives(&aqr_decompress::Native { threads: 1 })).unwrap(),
                short
            );
        }
        let noise = aqr_common::rng::noise(100_000);
        let (_, f) = compress_formula_as(&noise, kind_by_name(b"movie.mp4"), &opt);
        assert_eq!(f.streams[0].kind, Kind::Compressed);
        // but a format by its name is looked for as in a file of no name, as a scan finds what
        // the file starts with (and what is in it where that is turned down)
        let mut z = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::new(6));
        std::io::Write::write_all(&mut z, &text).unwrap();
        let gz = z.finish().unwrap();
        let opt = Options { smaller: false, ..opt };
        let (_, f) = compress_formula_as(&gz, kind_by_name(b"named.png"), &opt);
        assert!(!f.applies().is_empty());
    }

    /// Bytes of 16 values in no order (of an LCG): DEFLATE takes 4 bits off each.
    fn unruled(n: usize) -> Vec<u8> {
        let mut x = 1u64;
        (0..n)
            .map(|_| {
                x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                0x80 | (x >> 60) as u8
            })
            .collect()
    }

    /// A formula whose archive does not give the file back (a module's bug) is not taken: the
    /// file is archived as it is. So where the output differs only at its end, is shorter or
    /// longer (verified as it is decoded, part by part).
    #[test]
    fn not_given_back_raw() {
        let d: Vec<u8> = (0..300_000u32).map(|i| (i * 7 % 251) as u8).collect();
        let opt = Options::default();
        let mut wrong_end = d.clone();
        *wrong_end.last_mut().unwrap() ^= 1;
        for other in [d.clone(), wrong_end, d[..d.len() - 1].to_vec(), [&d[..], b"x"].concat()] {
            let mut f = Formula::default();
            f.push("a", other[..1000].to_vec());
            f.push("b", other[1000..].to_vec());
            f.root = Node::concat([Node::slice(0, 0, 1000), Node::slice(1, 0, other.len() as u64 - 1000)]);
            let cx = Cx::new(&d, &opt);
            let c = finished(f, &d, &opt, &cx);
            assert!(aqr_decompress::decompress(&c, 1).unwrap() == d);
        }
    }

    /// [`finish`] into memory, of as many bytes as it tells.
    fn finished(f: Formula, d: &[u8], opt: &Options, cx: &Cx) -> Vec<u8> {
        let mut c = Vec::new();
        let (n, _) = finish(f, d, opt, cx, Default::default(), None, false, None, &mut c).unwrap();
        assert_eq!(n, c.len());
        c
    }

    /// A GGUF file archived to a file directly ([`direct_room`]): its blocks written as they are
    /// made, its formula into the room before them, the archive the same whatever the threads
    /// and giving the file back; where the formula takes more than the room, the blocks moved
    /// after it. Archived to memory, or below the size, it is archived as before.
    #[test]
    fn gguf_direct() {
        let d = aqr_tensors::synth::file(3, 8);
        let path = std::env::temp_dir().join(format!("aqr-compress-direct-{}.aqr", std::process::id()));
        let archived = |threads: usize, min: usize, room: Option<u64>| {
            DIRECT_MIN.set(Some(min));
            DIRECT_ROOM.set(room);
            TENSORS_SEGMENT.set(Some(64 << 10));
            let opt = Options { threads, ..Options::default() };
            let mut file =
                std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(true).open(&path).unwrap();
            let before = DIRECTED.get();
            let (n, _) = compress_with(&d, Kind::Unknown, &opt, false, None, &mut file).unwrap();
            drop(file);
            let c = std::fs::read(&path).unwrap();
            assert_eq!(n, c.len());
            (c, DIRECTED.get() - before)
        };
        let (one, directed) = archived(1, 0, None);
        assert_eq!(directed, 1);
        assert!(aqr_decompress::decompress(&one, 1).unwrap() == d);
        let (four, _) = archived(4, 0, None);
        assert!(four == one, "the same whatever the threads");
        // (the room too small: the blocks moved after the formula)
        let (moved, _) = archived(2, 0, Some(2));
        assert!(moved.len() < one.len() && aqr_decompress::decompress(&moved, 1).unwrap() == d);
        let (held, directed) = archived(2, usize::MAX, None);
        assert_eq!(directed, 0);
        assert!(held != one && aqr_decompress::decompress(&held, 1).unwrap() == d);
        // (of the formula, not the file as it is: the room's spacer, or 2 bytes of it, more)
        assert!(one.len() < d.len() * 9 / 10 && moved.len() < held.len() + 64, "{} {}", one.len(), held.len());
        // (in memory: archived as before)
        DIRECT_MIN.set(Some(0));
        let opt = Options { threads: 2, ..Options::default() };
        let (mut c, before) = (Vec::new(), DIRECTED.get());
        compress_with(&d, Kind::Unknown, &opt, false, None, &mut c).unwrap();
        assert!(DIRECTED.get() == before && c == held);
        _ = std::fs::remove_file(&path);
    }

    /// An archive written to a file that does not give the file back is written again over it:
    /// the archive of the file raw, shorter than it, the file cut to its length.
    #[test]
    fn written_again_to_file() {
        let d = aqr_common::rng::noise(100_000);
        let opt = Options::default();
        // (twice as long as the file: given back longer)
        let mut f = Formula::default();
        f.push("a", d.clone());
        f.push("b", d.iter().map(|b| b ^ 0x5a).collect::<Vec<u8>>());
        let n = d.len() as u64;
        f.root = Node::concat([Node::slice(0, 0, n), Node::slice(1, 0, n)]);
        let path = std::env::temp_dir().join(format!("aqr-compress-written-again-{}.aqr", std::process::id()));
        let mut file =
            std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(true).open(&path).unwrap();
        let (n, _) = finish(f, &d, &opt, &Cx::new(&d, &opt), Default::default(), None, false, None, &mut file).unwrap();
        drop(file);
        let c = std::fs::read(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert_eq!(c.len(), n);
        assert!(c.len() < d.len() + 100);
        assert!(aqr_decompress::decompress(&c, 1).unwrap() == d);
    }

    /// The formulas recompressors make are evaluated beforehand: the archive is verified by the
    /// hashes recorded (not as a file it is not; without the records, by evaluating what they do
    /// not tell); decoding it takes the applications recorded as the file's from the file, and
    /// runs the module where they are not there.
    #[test]
    fn verified_beforehand() {
        // (of no records, which aqr-infer-recs would take, nor words, which aqr-wrt would)
        let text = unruled(240_000);
        let mut z = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::new(6));
        std::io::Write::write_all(&mut z, &text).unwrap();
        let d = z.finish().unwrap();
        let opt = Options { smaller: false, ..Options::default() };
        let mut cx = Cx::new(&d, &opt);
        cx.pre = Some(Arc::new(verify::Prevalidator::spawn(2, &[&d])));
        let f = recompress_at(&d, &opt, 0, Kind::Unknown, &cx);
        let memo = verify::Prevalidator::finish_shared(cx.pre.take().unwrap());
        assert!(!f.applies().is_empty());
        let c = cx.enc.encode(&f, Some(aqr::checksum(&d))).unwrap();
        let memo = std::cell::RefCell::new(memo);
        assert_eq!(hashed(&c, &d, &memo, &opt, false), Some(true));
        let mut other = d.clone();
        other[1000] ^= 1;
        assert_eq!(hashed(&c, &other, &memo, &opt, false), Some(false));
        // (without the records: evaluated where they do not tell)
        assert_eq!(hashed(&c, &d, &Default::default(), &opt, false), Some(true));
        let memo = memo.into_inner();
        let decode = |c: &[u8], memo: verify::Memo| {
            let files = |_: usize, range: Range<usize>| d.get(range).map(<[u8]>::to_vec);
            let m = verify::Cached::new(Native { threads: 1 }, memo, &files);
            let out = aqr::decode(c, &aqr_decompress::natives(&m));
            (out, m.hits.into_inner().unwrap())
        };
        let other = compress(&d[..d.len() - 1], &Options { verify: false, ..opt.clone() });
        // another archive's applications are not in it
        let (memo, hits) = {
            let files = |_: usize, range: Range<usize>| d.get(range).map(<[u8]>::to_vec);
            let m = verify::Cached::new(Native { threads: 1 }, memo, &files);
            aqr::decode(&other, &aqr_decompress::natives(&m)).unwrap();
            (m.memo.into_inner().unwrap(), m.hits.into_inner().unwrap())
        };
        assert_eq!(hits, 0);
        assert_eq!(decode(&c, memo), (Ok(d.clone()), f.applies().len()));
        // without the memo, the same, with the modules run
        assert_eq!(decode(&c, Default::default()), (Ok(d.clone()), 0));
    }

    /// A Split of a value made of another Split's outputs (a disk of fragmented files, cut into a
    /// DMG's chunks), its outputs taken by an application, is verified by its hashes: each value
    /// evaluated there is pulled of its own states of nodes of several outputs, not of those that
    /// another value already pulled.
    #[test]
    fn hashed_splits_of_splits() {
        let text = unruled(200_000);
        let mut z = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::new(6));
        std::io::Write::write_all(&mut z, &text).unwrap();
        let deflated = z.finish().unwrap();
        let tail = b"what follows the stream".to_vec();
        let d = [deflated.clone(), tail.clone()].concat();
        let opt = Options::default();
        let (profile, zlib) = opt.flate();
        let fopt =
            aqr_flate::Options { threads: 1, zlib, store: true, ..aqr_flate::Options::for_profile(profile).unwrap() };
        let parsed = aqr_flate::Parsed::head(&deflated, &fopt).unwrap();
        assert_eq!(parsed.len(), deflated.len());
        let (data, record, _) = parsed.deconstruct_bytes(&fopt, &Default::default()).unwrap();
        assert!(data == text);
        // the text in two pieces, the second first, and the tail: cut by a Split into the text
        // and the tail, the text inflated again
        // (the pieces across two blocks, as a file's content of several streams is)
        let (half, m) = (text.len() / 2, text.len() / 2 + 777);
        let rotated = [&text[half..], &text[..half]].concat();
        let mut f = Formula::default();
        f.push("raw", rotated[..m].to_vec());
        f.push("dmg", [&rotated[m..], &tail[..]].concat());
        let (n, m) = (text.len() as u64, m as u64);
        let pieces = Node::concat([Node::slice(0, 0, m), Node::slice(1, 0, n - m)]);
        let inner = f.split_node(pieces, vec![n - half as u64, half as u64]);
        let joined = Node::concat([inner[1].clone(), inner[0].clone(), Node::slice(1, n - m, tail.len() as u64)]);
        let outer = f.split_node(joined, vec![n, tail.len() as u64]);
        let records = f.push("flate/record", record.clone());
        let index = f.push("flate/index", aqr_flate::module::index(&[(data.len(), record.len(), deflated.len())]));
        let a = aqr_flate::formula::apply(
            profile,
            outer[0].clone(),
            records,
            index,
            deflated.len() as u64,
            aqr_flate::formula::Derived::None,
        )
        .unwrap();
        f.root = Node::concat([Node::Apply(Box::new(a)), outer[1].clone()]);
        assert!(f.eval(&Native::default()).unwrap() == d);
        let c = Cx::new(&d, &opt).enc.encode(&f, Some(aqr::checksum(&d))).unwrap();
        assert!(aqr_decompress::decompress(&c, 1).unwrap() == d);
        for threads in [1, 4] {
            assert_eq!(hashed(&c, &d, &Default::default(), &Options { threads, ..opt.clone() }, false), Some(true));
        }
    }

    /// An input that a recompressor would take more heap for than a recompression may is not
    /// recompressed so; one taken in parts is, whatever its size.
    #[test]
    fn too_large_kept() {
        let limit = 512 << 20;
        assert!(fits_within(Format::Png, 40 << 20, limit));
        assert!(!fits_within(Format::Png, 50 << 20, limit));
        assert!(fits_within(Format::Text, 400 << 20, limit) && !fits_within(Format::Text, 500 << 20, limit));
        assert!(fits_within(Format::Disk, usize::MAX >> 8, limit));
        assert!(fits_within(Format::Jsonl, usize::MAX >> 8, limit));
    }

    /// The records of applications of large outputs hold for other batches and archives: one
    /// whose formula is evaluated once though it is in several (`enwik9` and the entry of
    /// `enwik9.zip` in another batch) is verified by its hashes there too, not decoded.
    #[test]
    fn large_records_for_others() {
        let text = unruled(3_000_000);
        let mut z = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::new(6));
        std::io::Write::write_all(&mut z, &text).unwrap();
        let d = z.finish().unwrap();
        let opt = Options { smaller: false, ..Options::default() };
        let mut cx = Cx::new(&d, &opt);
        cx.pre = Some(Arc::new(verify::Prevalidator::spawn(2, &[&d])));
        let f = recompress_at(&d, &opt, 0, Kind::Unknown, &cx);
        drop(verify::Prevalidator::finish_shared(cx.pre.take().unwrap()));
        assert!(!f.applies().is_empty());
        let c = cx.enc.encode(&f, Some(aqr::checksum(&d))).unwrap();
        // (the memo of the batch it was evaluated in gone)
        assert_eq!(hashed(&c, &d, &Default::default(), &opt, false), Some(true));
    }

    /// A module run whose output is a whole file is run once: the same application again takes
    /// the file's bytes, but where the output is not the file.
    #[test]
    fn whole_files_run_once() {
        let text = unruled(240_000);
        let mut z = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::new(6));
        std::io::Write::write_all(&mut z, &text).unwrap();
        let d = z.finish().unwrap();
        let opt = Options { smaller: false, ..Options::default() };
        // (the DEFLATE stream, twice)
        let mut f = recompress(&d, &opt);
        let a = Node::Apply(Box::new(f.applies()[0].clone()));
        f.root = a.clone();
        let d = f.eval(&Native::default()).unwrap();
        f.root = Node::concat([a.clone(), a]);
        let c = Cx::new(&d, &opt).enc.encode(&f, None).unwrap();
        for (file, read) in [(d.clone(), 1), ([&d[..10], &[!d[10]], &d[11..]].concat(), 0)] {
            let reads = std::sync::atomic::AtomicUsize::new(0);
            let files = |_: usize, range: Range<usize>| {
                reads.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                file.get(range).map(<[u8]>::to_vec)
            };
            let mut m = verify::Cached::new(Native { threads: 1 }, Default::default(), &files);
            m.whole.insert(file.len(), vec![(0, xxhash_rust::xxh3::xxh3_128(&file))]);
            assert!(aqr::decode(&c, &aqr_decompress::natives(&m)).unwrap() == [&d[..], &d[..]].concat());
            assert_eq!(reads.into_inner(), read);
        }
    }

    /// A slice of a stream that is content is scanned wherever it is, as an input of an
    /// application that is not a content too; one of another stream is not.
    #[test]
    fn content_streams() {
        // (of no records, which aqr-infer-recs would take, nor words, which aqr-wrt would)
        let text = unruled(240_000);
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
            let children = nested(&mut f, &d, &opt, 0, &cx);
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
        f.mark_contents();
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
        assert_eq!(finished(wrong.clone(), &d, &Options { verify: false, ..opt.clone() }, &cx), raw);
        // two slices: the archive has the output's checksum
        wrong.root = Node::Concat(vec![Node::slice(0, 0, 100), Node::slice(0, 100, d.len() as u64 - 100)]);
        assert_eq!(finished(wrong.clone(), &d, &opt, &cx), raw);
        // a module that fails
        let fails = Formula {
            streams: vec![aqr_formula::Stream {
                class: "raw".into(),
                kind: Kind::Unknown,
                content: false,
                apart: false,
                data: vec![1, 2, 3].into(),
            }],
            root: Node::apply(FLATE, vec![], vec![Node::slice(0, 0, 3), Node::slice(0, 0, 3)], d.len() as u64),
            shared: vec![],
        };
        assert_eq!(finished(fails, &d, &opt, &cx), raw);
        // without verification, as it is
        let opt = Options { verify: false, ..opt };
        assert_eq!(
            aqr_decompress::decompress(&finished(wrong, &d, &opt, &cx), 1),
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
        let f = recompress(&d, &Options { smaller: false, ..Options::default() });
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
            h.extend_from_slice(&aqr_common::crc32::hash(&data).to_le_bytes());
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
        let jpeg = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../../codecs/aqr-jpeg/tests/data/gradient.jpg"))
            .unwrap();
        // files of different formats running to the end of the data, each turned down (one of
        // a format already turned down there would not be tried)
        let fakes: [&[u8]; 3] = [
            &[&b"\x89PNG\r\n\x1a\n"[..], &0x7fff_0000u32.to_be_bytes(), b"IHDR"].concat(),
            b"fLaC\x80\0\0\x22",
            b"%PDF-1.7\n",
        ];
        let data = |n: usize| [fakes[..n].concat(), vec![0; 10000], jpeg.clone()].concat();
        let found = |d: &[u8]| {
            let opt = Options { shared_modules: true, ..Options::default() };
            scan_with(d, None, &opt, 0, 0, &Cx::new(d, &opt)).map_or(0, |m| m.f.applies().len())
        };
        assert_eq!(found(&data(1)), 1);
        assert_eq!(found(&data(3)), 0);
    }
}
