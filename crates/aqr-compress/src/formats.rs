//! What [`detect`](crate::detect) recognises, each format in one place: its name, the names of
//! files of it, its recompressor, where a file of it ends, the heap it takes, the time it takes
//! and the level it is taken from ([`Spec`]). How a format is recognised by its bytes is
//! [`detect`](crate::detect)'s table of rows.

use std::collections::HashMap;
use std::sync::LazyLock;

use aqr_formula::{Kind, Region};

use crate::Recompressor::{self, Cx, Pure, PureWith};
use crate::extent::{self, clamp};

/// The parts of a file a recompressor turned down ([`Spec::regions`]).
type Regions = fn(&[u8]) -> Option<Vec<Region>>;

/// What is known of a format, besides how its bytes are recognised.
pub(crate) struct Spec {
    /// The recompressor that takes files of it.
    pub(crate) recompressor: Recompressor,
    /// Bytes of heap the recompressor takes at most for each byte of its input, as far as it
    /// holds what it makes of all of it (an estimate, rounded up: of its data decoded, its
    /// streams, its models), or none where it holds a bounded part of it at a time (chunks,
    /// segments, entries made apart, slices of the input). An input whose estimate is more
    /// than a recompression may take ([`crate::fits`]) is not recompressed so: data a
    /// recompressor cannot take in parts is kept as it is rather than letting it take all the
    /// memory there is. Of the peaks measured in docs/file-formats.md, "memory / input",
    /// rounded up; those not measured as the code holds them.
    pub(crate) heap: f64,
    /// The extensions of the names of files of it, lowercase ([`crate::kind_by_name`]).
    pub(crate) exts: &'static [&'static str],
    /// Bytes of the file at the start of the data, as far as its structure tells
    /// ([`crate::extent`]); by default all of it.
    pub(crate) extent: fn(&[u8]) -> Option<usize>,
    /// About the thread-milliseconds that making a MB of it takes: of the files of each
    /// format of 10gb (`--stats`, an M4; rounded), 50 for the others ([`crate::cost`]).
    pub(crate) cost: u64,
    /// The least [`Level::scale`](aqr_formula::aqr::Level::scale) that files of it are taken
    /// at ([`crate::format_on`]), as they are slow for their bytes; levels 0 and 1 take none.
    pub(crate) level: u8,
    /// The parts of a file of it that its recompressor turned down, to be looked into
    /// ([`Region`]).
    pub(crate) regions: Option<Regions>,
    /// It only bundles files: an archive of aqr-bundle's or aqr-asar's, or an image of
    /// aqr-disk's ([`Format::is_bundle`]).
    pub(crate) bundle: bool,
    /// It is text, of a recompressor of text ([`crate::is_text`]).
    pub(crate) text: bool,
}

fn whole(d: &[u8]) -> Option<usize> {
    Some(d.len())
}

impl Spec {
    /// The fields a format need not give.
    pub(crate) const DEFAULT: Spec = Spec {
        recompressor: Pure(|_, _| None),
        heap: 0.0,
        exts: &[],
        extent: whole,
        cost: 50,
        level: 0,
        regions: None,
        bundle: false,
        text: false,
    };
}

/// The kinds of data of the names of files of no format recompressed, by their extensions
/// ([`crate::kind_by_name`]).
const OTHER_EXTS: &[(Kind, &[&str])] = &[
    (
        Kind::Compressed,
        &[
            "zst", "br", "mp4", "m4a", "m4v", "ogv", "opus", "aac", "webp", "avif", "heic", "heif", "jxl", "jp2",
            "woff2",
        ],
    ),
    (Kind::Pixels, &["tga", "pbm"]),
    (
        Kind::Text,
        &[
            "txt", "log", "csv", "tsv", "json", "jsonl", "ndjson", "geojsonl", "xml", "html", "htm", "css", "js",
            "svg", "md",
        ],
    ),
    (Kind::Font, &["ttf", "otf", "ttc", "cff", "pfb"]),
];

/// The kind of data of each extension of [`Spec::exts`] and [`OTHER_EXTS`], each given once.
pub(crate) static BY_EXT: LazyLock<HashMap<&'static [u8], Kind>> = LazyLock::new(|| {
    let formats = Format::ALL.iter().map(|f| (Kind::Format(f.name()), f.spec().exts));
    let mut m = HashMap::new();
    for (kind, exts) in formats.chain(OTHER_EXTS.iter().copied()) {
        for e in exts {
            assert!(m.insert(e.as_bytes(), kind).is_none(), "extension {e} given twice");
        }
    }
    m
});

/// The formats, each `Variant = "name" { recompressor: …, heap: …, other fields of Spec }`:
/// the enum, [`Format::ALL`], [`Format::name`] and [`Format::spec`].
macro_rules! formats {
    ($(
        $(#[$doc:meta])*
        $v:ident = $name:literal { recompressor: $rec:expr, heap: $heap:expr $(, $field:ident: $val:expr)* $(,)? }
    )*) => {
        /// What [`detect`](crate::detect) recognises, by the recompressor that takes it.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum Format {
            $($(#[$doc])* $v,)*
        }

        impl Format {
            pub const ALL: [Format; [$(stringify!($v)),*].len()] = [$(Format::$v),*];

            pub fn name(self) -> &'static str {
                match self {
                    $(Format::$v => $name,)*
                }
            }

            /// What is known of the format.
            pub(crate) fn spec(self) -> &'static Spec {
                &SPECS[self as usize]
            }
        }

        static SPECS: [Spec; Format::ALL.len()] = [
            $(Spec { recompressor: $rec, heap: $heap, $($field: $val,)* ..Spec::DEFAULT },)*
        ];
    };
}

formats! {
    /// PE image of x86, x86-64 or ARM64 (aqr-exe on its executable sections).
    Pe = "pe" { recompressor: Cx(|d, _, opt, _, _| crate::exe::pe(d, opt)), heap: 9.0, extent: extent::pe, cost: 160 }
    /// Mach-O file, thin or universal (aqr-exe on the executable sections of its x86, x86-64
    /// and ARM64 images).
    MachO = "macho" {
        recompressor: Cx(|d, _, _, _, cx| {
            let r = cx.remaps.as_deref();
            crate::exe::macho(
                d,
                r.map_or(&[], |r| r.of(d)),
                r.map_or(&[], |r| r.deltas(d)),
                r.and_then(|r| r.data(d)),
            )
        }),
        heap: 9.0,
        extent: |d| aqr_exe::format::macho::extent(d).map(|e| clamp(d, e)),
    }
    /// ELF image, 32- or 64-bit, little-endian (aqr-exe on its code, of x86, x86-64 and
    /// ARM64).
    Elf = "elf" {
        recompressor: Cx(|d, _, _, _, cx| {
            let r = cx.remaps.as_deref();
            crate::exe::elf(d, r.map_or(&[], |r| r.of(d)), r.map_or(&[], |r| r.deltas(d)))
        }),
        heap: 9.0,
        extent: |d| aqr_exe::format::elf::extent(d).map(|e| clamp(d, e)),
        cost: 160,
    }
    /// gzip member (aqr-flate on its DEFLATE stream).
    Gzip = "gzip" {
        recompressor: Cx(crate::flate::gzip),
        // the data written out as it is decoded, and rebuilt in verification a batch at a time:
        // what is left is the tokens an adaptive profile keeps (6 bytes each; enwik8.gz at -8:
        // 7.6 per byte, 7.0 once the zlib detection and the chain rule stopped copying them)
        heap: 8.0,
        exts: &["gz", "tgz", "svgz"],
        extent: extent::gzip_members,
        cost: 370,
    }
    /// zlib stream (aqr-flate on its DEFLATE stream).
    Zlib = "zlib" {
        recompressor: Cx(crate::flate::zlib),
        // (as gzip's)
        heap: 8.0,
        extent: |d| crate::flate::zlib_deflate(d).map(|r| r.end + 4),
    }
    /// mozlz4 file or LZ4 frame (aqr-lz4 on its stream).
    Lz4 = "lz4" {
        recompressor: Pure(aqr_lz4::formula::encode),
        heap: 9.0,
        exts: &["jsonlz4", "mozlz4", "baklz4"],
        extent: aqr_lz4::formula::extent,
    }
    /// `.lzma` file (aqr-lzma on its stream), known by its header: not looked for in other data.
    Lzma = "lzma" {
        recompressor: Pure(aqr_lzma::formula::encode),
        heap: 12.0,
        exts: &["lzma", "tlz"],
        cost: 1370,
        level: 4,
    }
    /// `.xz` file (aqr-lzma on the LZMA2 streams of its blocks), known by its stream header.
    Xz = "xz" {
        recompressor: Pure(aqr_lzma::formula::encode_xz),
        heap: 12.0,
        exts: &["xz", "txz"],
        extent: aqr_lzma::formula::extent_xz,
        cost: 750,
        level: 4,
    }
    /// PPMd var. J file (aqr-ppmd: its header cut off, no module), known by its header:
    /// not looked for in other data.
    Ppmd = "ppmd" { recompressor: Pure(aqr_ppmd::formula::encode), heap: 0.0, exts: &["pmd"] }
    /// Matroska or WebM file (aqr-mkv: frames seen before held once, no module), known by its
    /// EBML header: not looked for in other data.
    Mkv = "mkv" {
        recompressor: Pure(aqr_mkv::formula::encode),
        // slices with an index of frames
        heap: 3.0,
        exts: &["webm", "mkv", "mka", "mk3d", "mks"],
        regions: Some(aqr_mkv::formula::regions),
    }
    /// AVI file with DivX 3 video (aqr-div3: the frames under a context model, module `div3-recons`), known by its
    /// header list: not looked for in other data.
    Div3 = "div3" {
        recompressor: Pure(aqr_div3::formula::encode),
        // the file, its skeleton and the model's output
        heap: 2.5,
        exts: &["avi"],
    }
    /// QuickTime or MXF file of ProRes frames (aqr-prores, the frames' coefficients under a
    /// model), known by its container's start and a frame near it: not looked for in other data.
    ProRes = "prores" {
        recompressor: Pure(aqr_prores::formula::encode),
        // in chunks
        heap: 0.0,
        // (ProRes's containers; others' video, as in MP4, stays compressed data)
        exts: &["mov", "mxf"],
        extent: aqr_prores::formula::extent,
    }
    /// lzop file (aqr-lzo on its LZO1X streams).
    Lzo = "lzo" {
        recompressor: Pure(aqr_lzo::formula::encode),
        heap: 9.0,
        exts: &["lzo", "tzo"],
        extent: aqr_lzo::formula::extent,
    }
    /// Stream of Apple's LZFSE and LZVN blocks (aqr-bvx on its data).
    Bvx = "bvx" {
        recompressor: Cx(|d, _, opt, _, cx| crate::bvx(d, opt, cx)),
        // the data decoded and its tokens kept (6 bytes each)
        heap: 36.0,
        exts: &["lzfse", "lzvn"],
        extent: aqr_bvx::formula::extent,
        level: 9,
    }
    /// bzip2 streams (aqr-bz2 on their data).
    Bzip2 = "bzip2" {
        // (where they may pay, see `bz2`)
        recompressor: PureWith(|d, p| crate::bz2(d, p.opt, p.depth, p.opt.threads, p.held)),
        heap: 9.0,
        exts: &["bz2", "tbz", "tbz2"],
        extent: aqr_bz2::formula::extent,
    }
    Png = "png" { recompressor: Pure(aqr_png::formula::encode), heap: 12.0, exts: &["png"], extent: extent::png }
    /// GIF file (aqr-gif on its images' LZW streams). Not known by its name: see
    /// [`crate::kind_by_name`].
    Gif = "gif" {
        recompressor: Pure(aqr_gif::formula::encode),
        heap: 12.0,
        extent: aqr_gif::formula::extent,
        cost: 1590,
        level: 4,
    }
    Jpeg = "jpeg" {
        recompressor: Pure(aqr_jpeg::formula::encode),
        heap: 9.0,
        exts: &["jpg", "jpeg", "jpe", "jfif"],
        extent: extent::jpeg,
    }
    /// Lossless JPEGs (SOF3) in a TIFF file (aqr-jpeg): a raw photograph in CR2 or DNG, known by
    /// its header and by a stream within it.
    JpegLossless = "jpeg-lossless" {
        recompressor: Pure(aqr_jpeg::lformula::encode),
        // (the samples of every stream held, as 16 bits, and the file without them)
        heap: 4.0,
        exts: &["cr2", "dng"],
        cost: 70,
    }
    Mp3 = "mp3" { recompressor: Pure(aqr_mp3::formula::encode), heap: 7.0, exts: &["mp3"], extent: extent::mp3 }
    Flac = "flac" {
        recompressor: Pure(aqr_flac::formula::encode),
        heap: 12.0,
        exts: &["flac"],
        extent: |d| Some(extent::flac_frames(d)),
    }
    /// Uncompressed samples in a trivial container (aqr-raw): WAV or AIFF anywhere, BMP, PNM,
    /// TIFF or DICOM at the start of a file.
    Raw = "raw" {
        recompressor: Pure(aqr_raw::formula::encode),
        heap: 12.0,
        exts: &["wav", "aif", "aiff", "aifc", "bmp", "pgm", "ppm", "pnm", "pam", "dcm"],
        extent: extent::raw,
    }
    /// PSD or PSB.
    Psd = "psd" {
        recompressor: Pure(aqr_psd::formula::encode),
        heap: 7.0,
        exts: &["psd", "psb"],
        extent: extent::psd,
        regions: Some(aqr_psd::formula::regions),
    }
    /// Clip Studio Paint file (aqr-clip), known by its header.
    Clip = "clip" { recompressor: Pure(aqr_clip::formula::encode), heap: 12.0, exts: &["clip"], extent: extent::clip }
    /// FMOD FSB5 sound bank (aqr-fsb), known by its header and samples' headers: at the start of
    /// data only.
    Fsb5 = "fsb5" {
        recompressor: Pure(aqr_fsb::formula::encode),
        heap: 7.0,
        exts: &["fsb"],
        extent: aqr_fsb::fsb5::extent,
    }
    /// Ogg file of Vorbis streams (aqr-ogg, and aqr-vorbis on their audio packets), known by its
    /// first page's CRC.
    Ogg = "ogg" {
        recompressor: Pure(aqr_ogg::formula::encode),
        heap: 7.0,
        exts: &["ogg", "oga"],
        extent: aqr_ogg::formula::extent,
    }
    /// DDS texture (aqr-tex): BC1 to BC5 blocks, or uncompressed pixels (aqr-raw's module).
    Dds = "dds" {
        recompressor: Pure(aqr_tex::formula::encode),
        heap: 12.0,
        exts: &["dds"],
        extent: |d| aqr_tex::dds::detect(d).map(|f| f.start + f.data.len() as usize),
    }
    Pdf = "pdf" {
        // (as `pdf_options` tells)
        recompressor: PureWith(|d, p| {
            aqr_pdf::formula::encode_with(d, &crate::pdf_options(p.opt, p.opt.threads))
        }),
        heap: 7.0,
        exts: &["pdf"],
        extent: |d| Some(extent::pdf(d)),
    }
    Sqlite = "sqlite" {
        recompressor: Pure(aqr_sqlite::formula::encode),
        heap: 9.0,
        exts: &["sqlite", "sqlite3"],
        extent: extent::sqlite,
    }
    /// LevelDB table (aqr-snappy on its blocks), known by its end.
    LevelDb = "leveldb" { recompressor: Pure(aqr_snappy::formula::encode), heap: 9.0, exts: &["ldb"] }
    /// ZIP archive (aqr-zip, and aqr-flate on its DEFLATE bodies).
    Zip = "zip" {
        recompressor: Cx(|d, _, opt, depth, cx| crate::zip::zip(d, opt, depth, cx)),
        // the bodies' data written out as it is decoded; in verification, zip-recons with the
        // bodies and the archive whole (enwik9.zip: 1.0); the tokens an adaptive profile keeps
        // (6 bytes each, 189 M for enwik9.zip, reserved at once as one for each byte of the
        // stream) at -7 to -9 (enwik9.zip, 322.6 MB: 5.05 GB = 15.7 per byte, 1.96 GB = 6.1 once
        // the detection and the chain rule stopped copying them), rounded up
        heap: 7.0,
        exts: &[
            "zip", "jar", "war", "ear", "apk", "aar", "xpi", "epub", "docx", "xlsx", "pptx", "odt", "ods", "odp",
            "whl", "nupkg",
        ],
        extent: aqr_zip::zip::extent,
        cost: 230,
    }
    /// 7z archive (aqr-7z, and aqr-lzma on its folders of LZMA or LZMA2), known by its
    /// signature header.
    SevenZip = "7z" {
        recompressor: Cx(|d, _, opt, _, _| crate::sevenz(d, opt)),
        heap: 12.0,
        exts: &["7z"],
        extent: aqr_7z::formula::extent,
        cost: 1750,
        level: 4,
    }
    /// CAB file (aqr-mszip, and aqr-flate on its MSZIP folders, aqr-lzx on its LZX folders),
    /// known by its header.
    Cab = "cab" {
        recompressor: Cx(|d, _, opt, _, cx| crate::cab(d, opt, cx)),
        heap: 9.0,
        exts: &["cab"],
        extent: aqr_mszip::formula::extent,
    }
    /// CHM file (aqr-mszip, and aqr-lzx on its LZX section), known by its header: at the start of
    /// data only. Not known by its name: one known so and turned down would not be scanned.
    Chm = "chm" {
        recompressor: Cx(|d, _, opt, _, cx| crate::chm(d, opt, cx)),
        heap: 9.0,
        level: crate::LZX_LEVEL,
    }
    /// ALZ archive (aqr-alz, and aqr-flate on its DEFLATE bodies, aqr-bz2 on its bzip2 bodies),
    /// known by its header and the record after it.
    Alz = "alz" {
        recompressor: Cx(|d, _, opt, depth, cx| crate::alz(d, opt, depth, cx)),
        heap: 9.0,
        exts: &["alz"],
        extent: aqr_alz::formula::extent,
    }
    /// InstallShield cabinet of data (`data1.cab`, aqr-isc, and aqr-flate on its DEFLATE chunks),
    /// known by its header: at the start of data only.
    Isc = "isc" {
        recompressor: Cx(|d, _, opt, _, cx| crate::isc(d, opt, cx)),
        heap: 9.0,
    }
    /// EGG archive (aqr-egg, and aqr-flate on its DEFLATE blocks, aqr-bz2 on its bzip2 blocks,
    /// aqr-lzma on its LZMA blocks), known by its header and the record after it.
    Egg = "egg" {
        recompressor: Cx(|d, _, opt, depth, cx| crate::egg(d, opt, depth, cx)),
        heap: 9.0,
        exts: &["egg"],
        extent: aqr_egg::formula::extent,
    }
    /// LHA archive (aqr-lha, which splits its streams of `-lh4-` to `-lh7-`), known by its first
    /// header (no signature at its start): at the start of data only.
    Lha = "lha" {
        recompressor: Cx(|d, _, opt, _, cx| crate::lha(d, opt, cx)),
        heap: 9.0,
        exts: &["lzh", "lha"],
        extent: aqr_lha::formula::extent,
    }
    /// RAR archive, RAR5 or RAR 1.5–4.x (aqr-rar, which splits its streams), known by its
    /// signature and headers: found in other data too (self-extracting programs).
    Rar = "rar" {
        recompressor: Cx(|d, _, opt, _, cx| crate::rar(d, opt, cx)),
        heap: 9.0,
        exts: &["rar"],
        extent: aqr_rar::formula::extent,
    }
    /// Aqueous archive, unencrypted and standalone: its blocks as they are, those of modules
    /// and resources shared with the archive holding it and the data of uncompressed blocks
    /// contents (`aqueous`). At the start of data only.
    Aqr = "aqr" {
        recompressor: Cx(|d, _, _, _, _| crate::aqueous::archive(d)),
        // slices of the input
        heap: 0.0,
        exts: &["aqr"],
    }
    /// git pack (aqr-git, and aqr-flate on its entries' zlib streams), a whole file: not
    /// looked for in other data.
    GitPack = "git-pack" {
        recompressor: Cx(|d, _, opt, depth, cx| crate::git::pack(d, opt, depth, cx)),
        heap: 12.0,
        extent: aqr_git::pack::extent,
    }
    /// FASTA file of nucleotide sequences (aqr-fasta), a whole file: not looked for in other
    /// data.
    Fasta = "fasta" {
        recompressor: Pure(aqr_fasta::formula::encode),
        // segments of 256 MiB, the encoder holding a whole segment (heap peaks of `aqr -c
        // --stats` with verification, hg19: chr21 49 MB -> 160 MB = 3.3 per byte, as a small
        // input takes its tables whole; chr1 254 MB -> 308 MB = 1.2; chr1+chr20 319 MB -> 482 MB
        // = 1.5, two segments), 2.0 for the largest ratio of the inputs that count
        heap: 2.0,
        exts: &["fa", "fasta", "fna", "ffn", "fas"],
    }
    /// Android DEX file (aqr-dex), known by its header: at the start of data only.
    Dex = "dex" {
        recompressor: Pure(aqr_dex::formula::encode),
        heap: 7.0,
        exts: &["dex"],
        extent: aqr_dex::formula::extent,
    }
    /// PDB file of the MSF 7.00 container (aqr-pdb), known by its magic: at the start of data only.
    Pdb = "pdb" {
        recompressor: Pure(aqr_pdb::formula::encode),
        heap: 7.0,
        exts: &["pdb"],
        extent: aqr_pdb::formula::extent,
    }
    /// Android resource table, `resources.arsc` (aqr-arsc), known by its table's header: at the
    /// start of data only.
    Arsc = "arsc" {
        recompressor: Pure(aqr_arsc::formula::encode),
        heap: 7.0,
        exts: &["arsc"],
        extent: aqr_arsc::formula::extent,
    }
    /// WebAssembly module (aqr-wasm), of the binary format's version 1.
    Wasm = "wasm" {
        recompressor: Pure(aqr_wasm::formula::encode),
        heap: 7.0,
        exts: &["wasm"],
        extent: aqr_wasm::formula::extent,
    }
    /// GNU gettext message catalog, `.mo` (aqr-gnu-gettext), known by its magic and header: at the
    /// start of data only.
    GnuGettext = "gnu-gettext" {
        recompressor: Pure(aqr_gnu_gettext::formula::encode),
        // coded whole (enwik9: 1.5)
        heap: 2.0,
        exts: &["mo", "gmo"],
        extent: aqr_gnu_gettext::formula::extent,
    }
    /// FITS file (aqr-fits), known by its primary header.
    Fits = "fits" {
        recompressor: Pure(aqr_fits::formula::encode),
        heap: 36.0,
        exts: &["fits", "fit", "fts", "fz"],
        extent: aqr_fits::formula::extent,
    }
    /// HDF5 file (aqr-hdf5, and aqr-flate on its deflated chunks), known by its superblock;
    /// after a user block, at the start of a file only.
    Hdf5 = "hdf5" {
        recompressor: Cx(|d, _, opt, depth, cx| crate::hdf5::file(d, opt, depth, cx)),
        heap: 9.0,
        exts: &["h5", "hdf5", "he5", "h5ad", "nwb"],
        extent: aqr_hdf5::formula::extent,
    }
    /// Wavefront OBJ file (aqr-obj), a whole file known by its text: not looked for in other
    /// data, nor by its name, which object files (COFF, OMF) have too.
    Obj = "obj" {
        recompressor: Pure(aqr_obj::formula::encode),
        // blocks of 64 MiB
        heap: 3.0,
    }
    /// ar archive (aqr-bundle).
    Ar = "ar" {
        recompressor: Cx(|d, _, _, _, _| crate::bundle::bundle(d)),
        // entries made apart
        heap: 0.0,
        extent: |d| extent::bundle(aqr_bundle::formula::Format::Ar, d),
        cost: 120,
        bundle: true,
    }
    /// tar archive (aqr-bundle), known by its first header's checksum: not looked for in other
    /// data.
    Tar = "tar" {
        recompressor: Cx(|d, _, _, _, _| crate::bundle::bundle(d)),
        heap: 0.0,
        exts: &["tar"],
        extent: |d| extent::bundle(aqr_bundle::formula::Format::Tar, d),
        bundle: true,
    }
    /// cpio archive (aqr-bundle); of the binary forms, not looked for in other data.
    Cpio = "cpio" {
        recompressor: Cx(|d, _, _, _, _| crate::bundle::bundle(d)),
        heap: 0.0,
        extent: |d| extent::bundle(aqr_bundle::formula::Format::Cpio, d),
        bundle: true,
    }
    /// Disk image (aqr-disk): a volume of a file system read (ISO 9660 or UDF, aqr-iso's, known
    /// by the descriptor at sector 16; FAT or exFAT, aqr-fat's, and NTFS, aqr-ntfs's, by the boot
    /// sector; ext2/3/4, aqr-ext's, by the superblock; APFS, aqr-apfs's, by the container's
    /// superblock), all of it or a partition of its MBR, GPT or
    /// APM, of the disk or of the virtual disk of a VHD, VHDX, VMDK, qcow2 or VDI container; or a
    /// container of compressed chunks (qcow2's compressed clusters, a streamOptimized VMDK, an
    /// EWF-E01 segment file: aqr-compress's `chunked`): at the start of data only.
    Disk = "disk" {
        recompressor: Cx(|d, _, opt, _, cx| crate::disk(d, opt, cx)),
        // slices of the input, or chunks
        heap: 0.0,
        exts: &["iso", "img", "vhd", "vhdx", "vmdk", "qcow2", "vdi"],
        bundle: true,
    }
    /// UDIF image (`.dmg`) of zlib's chunks (aqr-disk's `dmg`, aqr-compress's own formula: the
    /// disk a content), known by its trailer: all of the data only. (One of raw chunks is a
    /// container of [`Format::Disk`].)
    Dmg = "dmg" {
        recompressor: Cx(|d, _, opt, _, cx| crate::chunked::dmg(d, opt, cx)),
        // chunks
        heap: 0.0,
        exts: &["dmg"],
    }
    /// WIM file of LZX or of no compression (aqr-wim, and aqr-lzx on its chunks), known by its
    /// header: at the start of data only.
    Wim = "wim" {
        recompressor: Pure(crate::wim),
        // the data of each application made as it is needed
        heap: 0.0,
        exts: &["wim"],
    }
    /// Electron's asar archive (aqr-asar), known by its pickles and the start of its JSON: at the
    /// start of data only.
    Asar = "asar" {
        recompressor: Cx(|d, _, _, _, _| crate::bundle::asar(d)),
        // slices of the input
        heap: 0.0,
        exts: &["asar"],
        bundle: true,
    }
    /// Delimited text table (aqr-xsv): CSV, TSV, USV and the like, a whole file known by its
    /// text: not looked for in other data.
    Xsv = "xsv" {
        recompressor: Pure(aqr_xsv::formula::encode),
        // segments of 64 MiB parsed one at a time, the streams (0.6 per byte) whole (heap peaks
        // of `aqr -c --stats` on a synthetic CSV of nine columns: 67 MB 579 MB, 209 MB 671 MB =
        // 3.2 per byte, 628 MB 1.08 GB = 1.7; the tables of the segments are what there is for a
        // small input, 0.6 GB), 2.0 for the largest ratio of the inputs that count
        heap: 2.0,
        text: true,
    }
    /// JSON lines (aqr-jsonl), a whole file known by its text: not looked for in other data.
    Jsonl = "jsonl" {
        recompressor: Pure(aqr_jsonl::formula::encode),
        // segments
        heap: 0.0,
        text: true,
    }
    /// Text (aqr-wrt), a whole file of text bytes only: not looked for in other data.
    Text = "text" {
        // (with the dictionary given, if any)
        recompressor: PureWith(|d, p| match p.words {
            Some(w) => aqr_wrt::formula::encode_with(d, w, p.opt.threads),
            None => aqr_wrt::formula::encode(d, p.opt.threads),
        }),
        // the counts of the dictionary and the coded texts of the segments of 32 MiB being made,
        // which are spilled (heap peaks of `aqr -c --stats` with verification: enwik8 109 MB =
        // 1.09 per byte of the input, enwik9 601 MB = 0.60), enwik8's rounded up
        heap: 1.2,
        text: true,
    }
    /// 16-bit samples after a header telling their size (aqr-raw's `sized`), in data in which a
    /// scan finds no file: of no magic bytes, never detected.
    Sized = "sized" { recompressor: Pure(aqr_raw::formula::encode_sized), heap: 5.0 }
    /// Tables of records and chains of entries (aqr-infer-recs) in data in which a scan finds no
    /// file: of no magic bytes, never detected.
    Records = "records" { recompressor: Pure(aqr_infer_recs::formula::encode), heap: 5.0 }
}
