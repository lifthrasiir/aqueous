//! What [`compress`] and [`recompress`] are told ([`Options`]), and how deep formats nest
//! ([`Nest`]).

use super::*;

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
    /// The most bytes of description a formula may take ([`aqr_archive::description_bound`]): where
    /// the files found in data, or the contents of a file, would take more, those with the
    /// fewest bytes per byte of their description are kept raw. By default no limit.
    pub description: usize,
    /// The level of the archive of [`compress`] ([`aqr_archive::Encoder`]), and of the formulas sized
    /// for [`Options::smaller`]; and, by its [`Level::scale`], how hard data is recompressed:
    /// at 0 and 1 not at all (whatever [`Options::depth`]: at 0 every block uncompressed, at 1
    /// zstd alone), at 2 DEFLATE streams kept as they are ([`crate::inflated`]), from 6 on with
    /// the adaptive profile ([`Options::flate_profile`]) and from 7 on with no `probe`.
    pub level: Level,
    /// Keep a file raw, or the data where files are found, unless its formula archives
    /// smaller than it does raw ([`aqr_archive::Encoder::smaller`]).
    pub smaller: bool,
    /// The most bytes of data that DEFLATE streams (of gzip, zlib and ZIP) and streams of LZFSE
    /// and LZVN blocks inflate to, at every depth together, as a multiple of the file, on top of
    /// [`EXPANSION_SLACK`]: the streams past it are kept as they are.
    pub expansion: u64,
    /// Size formulas without their resources ([`aqr_archive::Encoder::share_resources`]), as for an
    /// archive of many files sharing them: for measuring what they come to in the limit.
    pub shared_resources: bool,
    /// Size formulas without their module blocks ([`aqr_archive::Encoder::share_modules`]), as an
    /// archive of several files does ([`compress_tree`]), whose modules are written once.
    pub shared_modules: bool,
    /// How each module is written into the archive ([`aqr_archive::Encoder::with_modules`]): by default
    /// its Wasm module, as aqr-modules built it ([`aqr_decompress::wasm_of`]); a self-extracting
    /// archive names those its program carries by their hashes alone ([`aqr_archive::Wasm::Registered`]).
    pub modules: aqr_archive::WasmOf,
    /// Of an archive of several files ([`compress_tree`]): the most bytes of a non-leaf block
    /// that files share (one stream longer is a block of its own), which a file read alone
    /// may have to decompress.
    pub block: usize,
    /// Of an archive of several files: compress the blocks alike of a batch (many of a class
    /// and kind of stream, small ones) with a dictionary trained for them where it pays
    /// ([`aqr_archive::Encoder::with_dictionaries`]). Off by default; it matters only with blocks of a
    /// few MiB or less ([`Options::block`]).
    pub dictionaries: bool,
    /// Begin the archive with a size hint block to be filled in, as the payload of an
    /// encrypted archive has to ([`aqr_archive::envelope::Sealer`], which fills it in).
    pub size_hint: bool,
    /// Give the archive a mirror block of each block whose damage would lose more than a file
    /// (spec, "Mirror block"; [`aqr_archive::Mirroring`]), for it to be repaired: as far from it as an
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
    /// Of an archive of several files: the contents (by index of the source) put first, in a
    /// segment of their own (and of the contents they are held together with): what a reader
    /// opens alone, the settings of a self-extracting program (docs/sfx-config-design.md, 2.1).
    /// None by default.
    pub lead: Vec<usize>,
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
    /// Of an archive of several files ([`compress_tree`]): what may be lost (docs/lossy-design.md,
    /// [`lossy`]): the files that a rewriter of it takes archived as files read the same, but
    /// for what their encoders chose ([`lossy::Class`]), and marked so ([`lossy::Mark`]). None,
    /// the default: every file as it is.
    pub lossy: Option<lossy::Lossy>,
}

/// The bytes of a segment of an archive of several files by default ([`Options::segment`]).
pub const SEGMENT: usize = 256 << 20;

aqr_common::zopt_namespace!("heap", "bytes of heap per input byte of each format's recompression (aqr-compress)");
aqr_common::zopt_namespace!("level", "least level each format is recompressed at (aqr-compress)");
aqr_common::zopt_namespace!(
    "cost",
    "thread-milliseconds per MB of each format, for the order of batches (aqr-compress)"
);
aqr_common::zopt_namespace!("format", "formats recognised, each can be turned off (aqr-compress)");
aqr_common::zopt_namespace!("bound", "application bounds of modules (aqr-compress)");
aqr_common::zopt_namespace!("memory", "the memory the formulas declare (aqr-compress)");
aqr_common::zopt_namespace!("table", "the choice of log, xsv and text for text (aqr-compress)");
aqr_common::zopt_namespace!("scan", "scans of data for files (aqr-compress)");
aqr_common::zopt_namespace!("dump", "dumps of what is found, to standard error (aqr-compress)");
aqr_common::zopt_namespace!("test", "behaviour for tests (aqr-compress)");

aqr_common::zopt! {
    /// Streams shorter than this are not taken as held by the files archived together ([`Held`]):
    /// what they share is found by compressing them solid.
    pub(crate) static SHARE_MIN: Opt<usize> = Opt::size(
        "share-min",
        256 << 10,
        "streams shorter are not taken as held by the files archived along",
    );
    /// Bytes that DEFLATE, LZFSE and LZVN streams may inflate to on top of [`Options::expansion`]
    /// times the file.
    pub static EXPANSION_SLACK: Opt<usize> = Opt::size(
        "expansion-slack",
        64 << 20,
        "bytes DEFLATE, LZFSE and LZVN streams may inflate to, besides expansion times the file",
    );
    /// [`Options::expansion`] by default.
    static EXPANSION: Opt<u64> =
        Opt::value("expansion", 32, "bytes per byte of the file that its DEFLATE, LZFSE and LZVN streams inflate to");
    /// [`Options::set_bytes`] by default.
    static SET_BYTES: Opt<usize> =
        Opt::size("set-bytes", 64 << 20, "most bytes of files coded against one another (a set)");
    /// [`Options::smaller`] by default.
    static SMALLER: Opt<bool> =
        Opt::flag("smaller", true, "keep a recompression only where its archive is smaller than the data's");
    /// [`Options::description`] by default: none, unlimited.
    static DESCRIPTION: Opt<Option<usize>> =
        Opt::size_or_off("description", None, "most bytes of a formula's description (unlimited unless given)");
}

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
    /// The aqr-flate profile that DEFLATE streams (of gzip, zlib and ZIP) are rebuilt with (zlib
    /// emulation is tried at every level): up to level 5, the default profile;
    /// from 6 on, the adaptive one with chain rules and zlib emulation (enwik8 by
    /// Info-ZIP 3.0 `-9`, 10 threads of a Ryzen 7 9700X: a record of 0.41% of the data rather
    /// than 2.6% with chain rules alone, split at 66 MB/s rather than 163, joined at 147 MB/s
    /// rather than 302; 2.8 KB with zlib emulation, split at 54 MB/s, joined at 165 MB/s, but at
    /// 25 MB/s on a thread: `enwik9.zip` alone some 40 s of decoding). Streams not of zlib's
    /// cost nothing more: detection turns them down at once. On 10gb, level 6 with it is 39 MB
    /// (1.6%) smaller than with the default profile, for 1.87 T instructions more (+20%) and a
    /// decoding of 43.6 s rather than 35.7; level 7 (no probe) 16 MB smaller again.
    pub fn flate_profile(&self) -> u16 {
        match self.level.scale() {
            ..6 => aqr_flate::PROFILE.get(),
            _ => aqr_flate::ADAPTIVE,
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
            description: DESCRIPTION.get().unwrap_or(usize::MAX),
            level: Level::MID,
            smaller: SMALLER.get(),
            expansion: EXPANSION.get(),
            shared_resources: false,
            shared_modules: false,
            modules: aqr_decompress::wasm_of,
            block: 16 << 20,
            dictionaries: false,
            size_hint: false,
            mirrors: false,
            set_bytes: SET_BYTES.get(),
            batch: 512 << 20,
            segment: SEGMENT,
            lead: Vec::new(),
            spill: None,
            application_bound: None,
            journal: None,
            lossy: None,
        }
    }
}
