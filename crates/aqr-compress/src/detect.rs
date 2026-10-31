//! Picking a recompressor by the magic bytes at the start of the data, and what data is by
//! the name of its file ([`kind_by_name`]).

use std::fmt;

use aqr_formula::Kind;

/// What [`detect`] recognises, by the recompressor that takes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Format {
    /// PE image of x86, x86-64 or ARM64 (aqr-exe on its executable sections).
    Pe,
    /// Mach-O file, thin or universal (aqr-exe on the executable sections of its x86, x86-64
    /// and ARM64 images).
    MachO,
    /// gzip member (aqr-flate on its DEFLATE stream).
    Gzip,
    /// zlib stream (aqr-flate on its DEFLATE stream).
    Zlib,
    /// mozlz4 file or LZ4 frame (aqr-lz4 on its stream).
    Lz4,
    /// `.lzma` file (aqr-lzma on its stream), known by its header: not looked for in other data.
    Lzma,
    /// `.xz` file (aqr-lzma on the LZMA2 streams of its blocks), known by its stream header.
    Xz,
    /// PPMd var. J file (aqr-ppmd: its header cut off, no module), known by its header:
    /// not looked for in other data.
    Ppmd,
    /// Matroska or WebM file (aqr-mkv: frames seen before held once, no module), known by its
    /// EBML header: not looked for in other data.
    Mkv,
    /// lzop file (aqr-lzo on its LZO1X streams).
    Lzo,
    /// Stream of Apple's LZFSE and LZVN blocks (aqr-bvx on its data).
    Bvx,
    /// bzip2 streams (aqr-bz2 on their data).
    Bzip2,
    Png,
    /// GIF file (aqr-gif on its images' LZW streams).
    Gif,
    Jpeg,
    Mp3,
    Flac,
    /// Uncompressed samples in a trivial container (aqr-raw): WAV or AIFF anywhere, BMP, PNM,
    /// TIFF or DICOM at the start of a file.
    Raw,
    /// PSD or PSB.
    Psd,
    /// Clip Studio Paint file (aqr-clip), known by its header.
    Clip,
    /// FMOD FSB5 sound bank (aqr-fsb), known by its header and samples' headers.
    Fsb5,
    /// Ogg file of Vorbis streams (aqr-ogg, and aqr-vorbis on their audio packets), known by its
    /// first page's CRC.
    Ogg,
    /// DDS texture (aqr-tex): BC1 to BC5 blocks, or uncompressed pixels (aqr-raw's module).
    Dds,
    Pdf,
    Sqlite,
    /// LevelDB table (aqr-snappy on its blocks), known by its end.
    LevelDb,
    /// ZIP archive (aqr-zip, and aqr-flate on its DEFLATE bodies).
    Zip,
    /// 7z archive (aqr-7z, and aqr-lzma on its folders of LZMA or LZMA2), known by its
    /// signature header.
    SevenZip,
    /// CAB file (aqr-mszip, and aqr-flate on its MSZIP folders, aqr-lzx on its LZX folders),
    /// known by its header.
    Cab,
    /// CHM file (aqr-mszip, and aqr-lzx on its LZX section), known by its header: at the start of
    /// data only.
    Chm,
    /// git pack (aqr-git, and aqr-flate on its entries' zlib streams), a whole file: not
    /// looked for in other data.
    GitPack,
    /// FASTA file of nucleotide sequences (aqr-fasta), a whole file: not looked for in other
    /// data.
    Fasta,
    /// WebAssembly module (aqr-wasm), of the binary format's version 1.
    Wasm,
    /// GNU gettext message catalog, `.mo` (aqr-gnu-gettext), known by its magic and header.
    GnuGettext,
    /// FITS file (aqr-fits), known by its primary header.
    Fits,
    /// HDF5 file (aqr-hdf5, and aqr-flate on its deflated chunks), known by its superblock;
    /// after a user block, at the start of a file only.
    Hdf5,
    /// Wavefront OBJ file (aqr-obj), a whole file known by its text: not looked for in other
    /// data, nor by its name, which object files (COFF, OMF) have too.
    Obj,
    /// ar archive (aqr-bundle).
    Ar,
    /// tar archive (aqr-bundle), known by its first header's checksum: not looked for in other
    /// data.
    Tar,
    /// cpio archive (aqr-bundle); of the binary forms, not looked for in other data.
    Cpio,
    /// ISO 9660 or UDF image (aqr-iso), known by the descriptor at sector 16: at the start of
    /// data only.
    Iso,
    /// WIM file of LZX or of no compression (aqr-wim, and aqr-lzx on its chunks), known by its
    /// header: at the start of data only.
    Wim,
    /// Electron's asar archive (aqr-asar), known by its pickles and the start of its JSON: at the
    /// start of data only.
    Asar,
    /// Delimited text table (aqr-xsv): CSV, TSV, USV and the like, a whole file known by its
    /// text: not looked for in other data.
    Xsv,
    /// Text (aqr-wrt), a whole file of text bytes only: not looked for in other data.
    Text,
    /// 16-bit samples after a header telling their size (aqr-raw's `sized`), in data in which a
    /// scan finds no file: of no magic bytes, never detected.
    Sized,
    /// Tables of records and chains of entries (aqr-infer-recs) in data in which a scan finds no
    /// file: of no magic bytes, never detected.
    Records,
}

impl Format {
    pub const ALL: [Format; 47] = [
        Format::Pe,
        Format::MachO,
        Format::Gzip,
        Format::Zlib,
        Format::Lz4,
        Format::Lzma,
        Format::Xz,
        Format::Ppmd,
        Format::Mkv,
        Format::Lzo,
        Format::Bvx,
        Format::Bzip2,
        Format::Png,
        Format::Gif,
        Format::Jpeg,
        Format::Mp3,
        Format::Flac,
        Format::Raw,
        Format::Psd,
        Format::Clip,
        Format::Fsb5,
        Format::Ogg,
        Format::Dds,
        Format::Pdf,
        Format::Sqlite,
        Format::LevelDb,
        Format::Zip,
        Format::SevenZip,
        Format::Cab,
        Format::Chm,
        Format::GitPack,
        Format::Fasta,
        Format::Wasm,
        Format::GnuGettext,
        Format::Fits,
        Format::Hdf5,
        Format::Obj,
        Format::Ar,
        Format::Tar,
        Format::Cpio,
        Format::Iso,
        Format::Wim,
        Format::Asar,
        Format::Xsv,
        Format::Text,
        Format::Sized,
        Format::Records,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Format::Pe => "pe",
            Format::MachO => "macho",
            Format::Gzip => "gzip",
            Format::Zlib => "zlib",
            Format::Lz4 => "lz4",
            Format::Lzma => "lzma",
            Format::Xz => "xz",
            Format::Ppmd => "ppmd",
            Format::Mkv => "mkv",
            Format::Lzo => "lzo",
            Format::Bvx => "bvx",
            Format::Bzip2 => "bzip2",
            Format::Png => "png",
            Format::Gif => "gif",
            Format::Jpeg => "jpeg",
            Format::Mp3 => "mp3",
            Format::Flac => "flac",
            Format::Raw => "raw",
            Format::Psd => "psd",
            Format::Clip => "clip",
            Format::Fsb5 => "fsb5",
            Format::Ogg => "ogg",
            Format::Dds => "dds",
            Format::Pdf => "pdf",
            Format::Sqlite => "sqlite",
            Format::LevelDb => "leveldb",
            Format::Zip => "zip",
            Format::SevenZip => "7z",
            Format::Cab => "cab",
            Format::Chm => "chm",
            Format::GitPack => "git-pack",
            Format::Fasta => "fasta",
            Format::Wasm => "wasm",
            Format::GnuGettext => "gnu-gettext",
            Format::Fits => "fits",
            Format::Hdf5 => "hdf5",
            Format::Obj => "obj",
            Format::Ar => "ar",
            Format::Tar => "tar",
            Format::Cpio => "cpio",
            Format::Iso => "iso",
            Format::Wim => "wim",
            Format::Asar => "asar",
            Format::Xsv => "xsv",
            Format::Text => "text",
            Format::Sized => "sized",
            Format::Records => "records",
        }
    }

    /// Whether it only bundles files: an archive of aqr-bundle's or aqr-asar's, or an image of
    /// aqr-iso's.
    pub fn is_bundle(self) -> bool {
        matches!(self, Format::Ar | Format::Tar | Format::Cpio | Format::Iso | Format::Asar)
    }

    /// The format of [`Format::name`] `name`, as [`Kind::Format`] names it.
    pub fn from_name(name: &str) -> Option<Format> {
        Format::ALL.into_iter().find(|f| f.name() == name)
    }
}

/// What the data of a file named `name` (a path, of `/` or `\` separators) is by its
/// extension, case aside: a file of a format recompressed ([`Kind::Format`]), already
/// compressed (audio, video, images and archives that no recompressor takes), uncompressed
/// images (pixels after a header), text or fonts. Anything else is [`Kind::Unknown`],
/// executables too (an installer has an archive after its image), WOFF fonts (of zlib
/// streams), and GIF files, whose LZW compresses poorly (some writers' animations come to a
/// sixth at zstd -3): aqr-gif takes them by their magic bytes, and those it turns down are
/// compressed as they are.
pub fn kind_by_name(name: &[u8]) -> Kind {
    let base = name.rsplit(|&b| b == b'/' || b == b'\\').next().unwrap_or(name);
    let Some(dot) = base.iter().rposition(|&b| b == b'.').filter(|&p| p > 0) else { return Kind::Unknown };
    let ext = base[dot + 1..].to_ascii_lowercase();
    let format = |f: Format| Kind::Format(f.name());
    match &ext[..] {
        b"png" => format(Format::Png),
        b"jpg" | b"jpeg" | b"jpe" | b"jfif" => format(Format::Jpeg),
        b"mp3" => format(Format::Mp3),
        b"flac" => format(Format::Flac),
        b"wav" | b"aif" | b"aiff" | b"aifc" | b"bmp" | b"pgm" | b"ppm" | b"pnm" | b"pam" | b"dcm" => {
            format(Format::Raw)
        }
        b"psd" | b"psb" => format(Format::Psd),
        b"clip" => format(Format::Clip),
        b"fsb" => format(Format::Fsb5),
        b"ogg" | b"oga" => format(Format::Ogg),
        b"dds" => format(Format::Dds),
        b"pdf" => format(Format::Pdf),
        b"gz" | b"tgz" | b"svgz" => format(Format::Gzip),
        b"sqlite" | b"sqlite3" => format(Format::Sqlite),
        b"fa" | b"fasta" | b"fna" | b"ffn" | b"fas" => format(Format::Fasta),
        b"mo" | b"gmo" => format(Format::GnuGettext),
        b"wasm" => format(Format::Wasm),
        b"fits" | b"fit" | b"fts" | b"fz" => format(Format::Fits),
        b"h5" | b"hdf5" | b"he5" | b"h5ad" | b"nwb" => format(Format::Hdf5),
        b"ldb" => format(Format::LevelDb),
        b"tar" => format(Format::Tar),
        b"iso" => format(Format::Iso),
        b"wim" => format(Format::Wim),
        b"asar" => format(Format::Asar),
        b"jsonlz4" | b"mozlz4" | b"baklz4" => format(Format::Lz4),
        b"lzma" | b"tlz" => format(Format::Lzma),
        b"xz" | b"txz" => format(Format::Xz),
        b"pmd" => format(Format::Ppmd),
        b"webm" | b"mkv" | b"mka" | b"mk3d" | b"mks" => format(Format::Mkv),
        b"lzo" | b"tzo" => format(Format::Lzo),
        b"lzfse" | b"lzvn" => format(Format::Bvx),
        b"bz2" | b"tbz" | b"tbz2" => format(Format::Bzip2),
        b"zip" | b"jar" | b"war" | b"ear" | b"apk" | b"aar" | b"xpi" | b"epub" | b"docx" | b"xlsx" | b"pptx"
        | b"odt" | b"ods" | b"odp" | b"whl" | b"nupkg" => format(Format::Zip),
        b"7z" => format(Format::SevenZip),
        b"cab" => format(Format::Cab),
        b"zst" | b"br" | b"rar" | b"mp4" | b"m4a" | b"m4v" | b"mov" | b"ogv" | b"opus" | b"aac" | b"webp" | b"avif"
        | b"heic" | b"heif" | b"jxl" | b"jp2" | b"woff2" => Kind::Compressed,
        b"tga" | b"pbm" => Kind::Pixels,
        b"txt" | b"log" | b"csv" | b"tsv" | b"json" | b"xml" | b"html" | b"htm" | b"css" | b"js" | b"svg" | b"md" => {
            Kind::Text
        }
        b"ttf" | b"otf" | b"ttc" | b"cff" | b"pfb" => Kind::Font,
        _ => Kind::Unknown,
    }
}

impl fmt::Display for Format {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// The format `d` starts like, if any. Only the first bytes are looked at (and the PE
/// header, Mach-O load commands and universal headers, the end of an ID3v2 tag), except for a
/// LevelDB table, known by its footer: the recompressor may still turn the data down.
pub fn detect(d: &[u8]) -> Option<Format> {
    detect_at(d, true)
}

/// [`detect`], of data at its start (`start`) or in the middle of other data.
fn detect_at(d: &[u8], start: bool) -> Option<Format> {
    let at = |p: usize, m: &[u8]| d.get(p..p + m.len()) == Some(m);
    if at(0, b"\x89PNG\r\n\x1a\n") {
        Some(Format::Png)
    } else if aqr_gif::formula::starts(d) {
        Some(Format::Gif)
    } else if start && aqr_iso::starts(d) {
        // (before the others: a hybrid image starts with a boot sector, or anything)
        Some(Format::Iso)
    } else if start && aqr_wim::wim::starts(d) {
        Some(Format::Wim)
    } else if start && aqr_asar::starts(d) {
        Some(Format::Asar)
    } else if start && aqr_mszip::formula::starts_chm(d) {
        Some(Format::Chm)
    } else if sqlite_header(d) {
        Some(Format::Sqlite)
    } else if at(0, b"%PDF-") {
        Some(Format::Pdf)
    } else if at(0, b"8BPS") {
        Some(Format::Psd)
    } else if at(0, b"CSFCHUNK") {
        Some(Format::Clip)
    } else if aqr_fsb::fsb5::starts(d) {
        Some(Format::Fsb5)
    } else if aqr_ogg::formula::starts(d) {
        Some(Format::Ogg)
    } else if at(0, b"fLaC") {
        Some(Format::Flac)
    } else if at(0, b"RIFF") && at(8, b"WAVE") || at(0, b"FORM") && (at(8, b"AIFF") || at(8, b"AIFC")) {
        Some(Format::Raw)
    } else if let Some(p) = id3_len(d) {
        // ID3v2 is found before either; what follows the tag tells them apart.
        Some(if at(p, b"fLaC") { Format::Flac } else { Format::Mp3 })
    } else if at(0, &[0xff, 0xd8, 0xff]) {
        Some(Format::Jpeg)
    } else if mp3_header(d) {
        Some(Format::Mp3)
    } else if aqr_tex::dds::starts(d) {
        Some(Format::Dds)
    } else if pe(d) {
        Some(Format::Pe)
    } else if aqr_exe::format::macho::starts(d) {
        Some(Format::MachO)
    } else if aqr_lz4::formula::starts(d) {
        Some(Format::Lz4)
    } else if aqr_lzo::formula::starts(d) {
        Some(Format::Lzo)
    } else if aqr_bvx::formula::starts(d) {
        Some(Format::Bvx)
    } else if aqr_bz2::formula::starts(d) {
        Some(Format::Bzip2)
    } else if aqr_lzma::formula::starts_xz(d) {
        Some(Format::Xz)
    } else if at(0, b"PK\x03\x04") {
        Some(Format::Zip)
    } else if aqr_7z::formula::starts(d) {
        Some(Format::SevenZip)
    } else if aqr_mszip::formula::starts(d) {
        Some(Format::Cab)
    } else if start && aqr_git::pack::starts(d) {
        Some(Format::GitPack)
    } else if at(0, &[0x1f, 0x8b, 8]) {
        Some(Format::Gzip)
    } else if zlib_header(d, start) {
        Some(Format::Zlib)
    } else if aqr_snappy::leveldb::is_table(d) {
        Some(Format::LevelDb)
    } else if start
        && ([&b"BM"[..], b"P5", b"P6", b"P7", b"II*\0", b"MM\0*", &[8, 0]].iter().any(|m| at(0, m)) || at(128, b"DICM"))
        && aqr_raw::container::detect(d).is_some()
    {
        // Two bytes (or DICOM's group 8) are too weak a signature to look for in other data.
        Some(Format::Raw)
    } else if aqr_wasm::formula::starts(d) {
        Some(Format::Wasm)
    } else if aqr_gnu_gettext::formula::starts(d) {
        Some(Format::GnuGettext)
    } else if aqr_fits::formula::starts(d) {
        Some(Format::Fits)
    } else if aqr_hdf5::formula::starts(d) || start && aqr_hdf5::formula::after_user_block(d) {
        Some(Format::Hdf5)
    } else if start && aqr_fasta::formula::starts(d) {
        Some(Format::Fasta)
    } else if start && aqr_obj::formula::starts(d) {
        Some(Format::Obj)
    } else if aqr_bundle::ar::starts(d) {
        Some(Format::Ar)
    } else if (start || d.starts_with(b"07070")) && aqr_bundle::cpio::starts(d) {
        Some(Format::Cpio)
    } else if start && aqr_bundle::tar::starts(d) {
        Some(Format::Tar)
    } else if start && aqr_lzma::formula::starts(d) {
        Some(Format::Lzma)
    } else if start && aqr_ppmd::starts(d) {
        // (four bytes are too weak a signature to look for in other data)
        Some(Format::Ppmd)
    } else if start && aqr_mkv::starts(d) {
        Some(Format::Mkv)
    } else if start && aqr_xsv::formula::starts(d) {
        Some(Format::Xsv)
    } else if start && aqr_wrt::formula::starts(d) {
        Some(Format::Text)
    } else {
        None
    }
}

/// Whether a file of a format [`starts`] finds may start with bytes `a`, `b`.
const fn pair(a: u8, b: u8) -> bool {
    match a {
        0x89 => b == b'P' || b == b'L' || b == b'H',
        b'%' => b == b'P',
        b'f' => b == b'L',
        b'R' => b == b'I',
        b'F' => b == b'O' || b == b'S',
        b'I' => b == b'D',
        b'D' => b == b'D',
        0xff => b == 0xd8 || b & 0xe0 == 0xe0,
        b'M' => b == b'Z' || b == b'S',
        // Mach-O, little-endian (32- or 64-bit), and universal
        0xce | 0xcf => b == 0xfa,
        0xca => b == 0xfe,
        0x1f => b == 0x8b,
        b'm' => b == b'o',
        0x04 => b == 0x22,
        b'P' => b == b'K',
        b'C' => b == b'S',
        b'!' => b == b'<',
        b'0' => b == b'7',
        b'S' => b == b'I',
        0xfd => b == b'7',
        b'7' => b == b'z',
        0x00 => b == b'a',
        b'b' => b == b'v',
        b'G' => b == b'I',
        b'O' => b == b'g',
        b'B' => b == b'Z',
        // zlib's CMF and FLG: DEFLATE with a window of at most 32K, no preset dictionary
        _ => {
            a & 15 == 8 && a >> 4 <= 7 && b & 0x20 == 0 && (a as u16 * 256 + b as u16).is_multiple_of(31)
                || a == b'8' && b == b'B'
        }
    }
}

/// [`pair`] for every two bytes, a bit each.
const PAIRS: [u64; 1024] = {
    let mut t = [0u64; 1024];
    let mut i = 0;
    while i < 65536 {
        if pair((i >> 8) as u8, i as u8) {
            t[i >> 6] |= 1 << (i & 63);
        }
        i += 1;
    }
    t
};

fn pairs(a: u8, b: u8) -> bool {
    let i = (a as usize) << 8 | b as usize;
    PAIRS[i >> 6] >> (i & 63) & 1 != 0
}

/// Where in `d` from `p` on a file of a format [`starts`] finds may start (by its first two
/// bytes), or its end.
pub(crate) fn next_start(d: &[u8], p: usize) -> usize {
    d.get(p..).and_then(|r| r.windows(2).position(|w| pairs(w[0], w[1]))).map_or(d.len(), |q| p + q)
}

/// [`detect`] for a file found anywhere in data. Two first bytes no signature starts with are
/// turned down at once, and so are a LevelDB table, whose end would be that of the data, and a
/// SQLite database, which is a whole file and is not in other data. A zlib stream starting with
/// a fixed block is not taken either ([`zlib_header`]).
pub(crate) fn starts(d: &[u8]) -> Option<Format> {
    // what every format starting with those bytes starts with: most pairs start none, and some
    // that are common in code (0x48 0x89, x86-64's `mov`) only a zlib header
    if !pairs(*d.first()?, *d.get(1)?) {
        return None;
    }
    let likely = match d[0] {
        0x89 => d.starts_with(b"\x89PNG") || d.starts_with(&aqr_lzo::lzop::MAGIC) || aqr_hdf5::formula::starts(d),
        b'%' => d.starts_with(b"%PDF-"),
        b'8' => d.starts_with(b"8BPS") || zlib_header(d, false),
        b'f' => d.starts_with(b"fLaC"),
        b'R' => d.starts_with(b"RIFF"),
        b'F' => d.starts_with(b"FORM"),
        b'I' => d.starts_with(b"ID3"),
        b'D' => aqr_tex::dds::starts(d),
        0xff => true,
        b'M' => d.starts_with(b"MZ") || aqr_mszip::formula::starts(d),
        0xce | 0xcf => d.starts_with(&[d[0], 0xfa, 0xed, 0xfe]),
        0xca => d.starts_with(&[0xca, 0xfe, 0xba]),
        0x1f => d.starts_with(&[0x1f, 0x8b]),
        b'm' | 0x04 => aqr_lz4::formula::starts(d),
        b'P' => d.starts_with(b"PK\x03\x04"),
        b'C' => d.starts_with(b"CSFCHUNK"),
        b'!' => d.starts_with(aqr_bundle::ar::MAGIC),
        b'0' => d.starts_with(b"07070"),
        b'S' => aqr_fits::formula::starts(d),
        0xfd => aqr_lzma::formula::starts_xz(d),
        b'7' => aqr_7z::formula::starts(d),
        0x00 => aqr_wasm::formula::starts(d),
        b'b' => aqr_bvx::formula::starts(d),
        b'G' => aqr_gif::formula::starts(d),
        b'O' => aqr_ogg::formula::starts(d),
        b'B' => aqr_bz2::formula::starts(d),
        // zlib's CMF: DEFLATE with a window of at most 32K
        b if b & 15 == 8 && b >> 4 <= 7 => zlib_header(d, false),
        _ => false,
    };
    if likely { detect_at(d, false).filter(|&f| !matches!(f, Format::LevelDb | Format::Sqlite)) } else { None }
}

/// A SQLite database header: the magic string, a page size, and the fields that must have one
/// value (the file format's versions 1 or 2, payload fractions 64, 32 and 32). SQLite's code
/// holds the magic string too.
pub(crate) fn sqlite_header(d: &[u8]) -> bool {
    let Some(h) = d.first_chunk::<24>().filter(|h| h.starts_with(b"SQLite format 3\0")) else { return false };
    let size = u16::from_be_bytes([h[16], h[17]]);
    (size == 1 || size.is_power_of_two() && (512..=32768).contains(&size))
        && (1..=2).contains(&h[18])
        && (1..=2).contains(&h[19])
        && h[21..24] == [64, 32, 32]
}

/// Bytes of the ID3v2 tag at the start of `d`, if any.
pub(crate) fn id3_len(d: &[u8]) -> Option<usize> {
    let h = d.first_chunk::<10>().filter(|h| h.starts_with(b"ID3") && h[6..].iter().all(|&b| b < 0x80))?;
    let size = h[6..].iter().fold(0usize, |a, &b| a << 7 | b as usize);
    Some(10 + size + if h[5] & 0x10 != 0 { 10 } else { 0 })
}

/// An MPEG audio layer III frame header.
pub(crate) fn mp3_header(d: &[u8]) -> bool {
    let &[0xff, b1, b2, ..] = d else { return false };
    let (sync, version, layer) = (b1 >> 5, b1 >> 3 & 3, b1 >> 1 & 3);
    let (bitrate, rate) = (b2 >> 4, b2 >> 2 & 3);
    sync == 7 && version != 1 && layer == 1 && bitrate != 15 && rate != 3
}

/// "MZ" with `e_lfanew` pointing at "PE\0\0".
pub(crate) fn pe(d: &[u8]) -> bool {
    let Some(&[b'M', b'Z']) = d.first_chunk() else { return false };
    let Some(&lfanew) = d.get(0x3c..0x40).and_then(|s| s.first_chunk::<4>()) else { return false };
    let p = u32::from_le_bytes(lfanew) as usize;
    p >= 0x40 && d.get(p..p.saturating_add(4)) == Some(b"PE\0\0")
}

/// A zlib header (RFC 1950) for DEFLATE without a preset dictionary, followed by what starts
/// like a DEFLATE stream ([`aqr_flate::inflate::plausible_start`]), with a fixed block only at
/// the `start` of data.
pub(crate) fn zlib_header(d: &[u8], start: bool) -> bool {
    let Some(&[cmf, flg]) = d.first_chunk() else { return false };
    if !(cmf & 15 == 8 && cmf >> 4 <= 7 && flg & 0x20 == 0 && (cmf as u16 * 256 + flg as u16).is_multiple_of(31)) {
        return false;
    }
    // Two bytes pass by chance in one of about 250 places, and more often in code (`48 89`,
    // x86-64's `mov`): the stream must start like one too.
    aqr_flate::inflate::plausible_start(&d[2..], start)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The first two bytes of anything `starts` takes are a pair; noise has few.
    #[test]
    fn pairs_of_starts() {
        let n = (0..65536).filter(|&i| pairs((i >> 8) as u8, i as u8)).count();
        assert!(n < 400, "{n}");
        for (d, _) in [
            (&b"\x89PNG"[..], 0),
            (b"%PDF", 0),
            (b"8BPS", 0),
            (b"fLaC", 0),
            (b"RIFF", 0),
            (b"FORM", 0),
            (b"ID3\x04", 0),
            (b"\xff\xd8", 0),
            (b"\xff\xfb", 0),
            (b"MZ", 0),
            (b"\xcf\xfa", 0),
            (b"\xce\xfa", 0),
            (b"\xca\xfe", 0),
            (b"\x1f\x8b", 0),
            (b"mozLz4", 0),
            (b"\x04\x22", 0),
            (b"\x89LZO", 0),
            (b"PK", 0),
            (b"\x78\x9c", 0),
            (b"\x78\xda", 0),
            (b"!<", 0),
            (b"07", 0),
            (b"SI", 0),
            (b"\x48\x89", 0),
            (b"bvx2", 0),
            (b"BZh9", 0),
            (b"OggS", 0),
        ] {
            assert!(pairs(d[0], d[1]), "{d:x?}");
        }
    }

    /// `starts` turns down at once only what `detect_at` would turn down in the middle of data
    /// (or take as a whole file alone), at every position of data of every kind.
    #[test]
    fn starts_as_detect() {
        let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
        let mut data = aqr_common::rng::noise(20000);
        for f in [
            "codecs/aqr-jpeg/tests/data/gradient.jpg",
            "codecs/aqr-flac/tests/data/s16_f5.flac",
            "codecs/aqr-mp3/tests/data/tags.mp3",
            "codecs/aqr-zip/tests/data/py-deflate.zip",
            "codecs/aqr-zip/tests/data/infozip-stream.zip",
            "codecs/aqr-snappy/tests/data/table-mul.ldb",
            "codecs/aqr-ogg/tests/data/chained.ogg",
        ] {
            data.extend(std::fs::read(format!("{root}/{f}")).unwrap());
        }
        // every signature, and every byte before a header-like pair
        for b in 0..=255u8 {
            data.extend([b, 0x9c, 0x03, 0x00, b, 0x89, 0xe5, 0x41, b, 0xfb, 0x90, 0]);
        }
        for m in
            [&b"\x89PNG\r\n\x1a\n"[..], b"%PDF-1.7", b"8BPS\0\x01", b"RIFF\0\0\0\0WAVE", b"ID3\x04\0\0\0\0\0\x02xx"]
        {
            data.extend_from_slice(m);
        }
        for p in 0..data.len() {
            let d = &data[p..];
            assert_eq!(
                starts(d),
                detect_at(d, false).filter(|&f| !matches!(f, Format::LevelDb | Format::Sqlite)),
                "at {p}"
            );
        }
    }

    /// Kinds by the extension of the file's name, case aside, whatever its directories.
    #[test]
    fn kinds_by_name() {
        for (name, kind) in [
            (&b"photo.JPG"[..], Kind::Format("jpeg")),
            (b"dir.d/sub\\song.flac", Kind::Format("flac")),
            (b"song.OGG", Kind::Format("ogg")),
            (b"song.opus", Kind::Compressed),
            (b"a/b/places.sqlite", Kind::Format("sqlite")),
            (b"art/page0001.CLIP", Kind::Format("clip")),
            (b"lib.tar.gz", Kind::Format("gzip")),
            (b"src.tar.bz2", Kind::Format("bzip2")),
            (b"movie.mp4", Kind::Compressed),
            (b"clip.webm", Kind::Format("mkv")),
            (b"movie.MKV", Kind::Format("mkv")),
            (b"notes.txt", Kind::Text),
            (b"var/log/access.log", Kind::Text),
            (b"scan.BMP", Kind::Format("raw")),
            (b"take.wav", Kind::Format("raw")),
            (b"sky.tga", Kind::Pixels),
            (b"fonts/a.otf", Kind::Font),
            (b"fonts/a.woff", Kind::Unknown),
            (b"setup.exe", Kind::Unknown),
            // a Wavefront OBJ file or an object file
            (b"mesh.obj", Kind::Unknown),
            (b".png", Kind::Unknown),
            (b"dir.png/file", Kind::Unknown),
            (b"README", Kind::Unknown),
            (b"", Kind::Unknown),
        ] {
            assert_eq!(kind_by_name(name), kind, "{}", String::from_utf8_lossy(name));
        }
        // every format named is one known
        for f in Format::ALL {
            assert_eq!(Format::from_name(f.name()), Some(f));
        }
        assert_eq!(Format::from_name("tgz"), None);
    }

    #[test]
    fn magics() {
        let mut pe = vec![0u8; 0x84];
        pe[..2].copy_from_slice(b"MZ");
        pe[0x3c] = 0x80;
        pe[0x80..].copy_from_slice(b"PE\0\0");
        // a 64-bit Mach-O header with one load command, and a universal header of it
        let mut macho = vec![0u8; 40];
        macho[..4].copy_from_slice(&[0xcf, 0xfa, 0xed, 0xfe]);
        macho[16] = 1;
        macho[20] = 8;
        let mut fat = vec![0u8; 0x1000];
        fat[..8].copy_from_slice(&[0xca, 0xfe, 0xba, 0xbe, 0, 0, 0, 1]);
        fat[8..28].copy_from_slice(&[1, 0, 0, 7, 0, 0, 0, 3, 0, 0, 0x10, 0, 0, 0, 0, 40, 0, 0, 0, 12]);
        fat.extend_from_slice(&macho);
        let mut id3_flac = b"ID3\x04\0\0\0\0\0\x02xx".to_vec();
        id3_flac.extend_from_slice(b"fLaC");
        let cases: &[(&[u8], Option<Format>)] = &[
            (b"\x89PNG\r\n\x1a\n....", Some(Format::Png)),
            (b"%PDF-1.7\n", Some(Format::Pdf)),
            (b"8BPS\0\x01", Some(Format::Psd)),
            (b"8BPS\0\x02", Some(Format::Psd)),
            (b"CSFCHUNK\0\0\0\0", Some(Format::Clip)),
            (b"fLaC\0\0\0\x22", Some(Format::Flac)),
            (b"RIFF\0\0\0\0WAVEfmt ", Some(Format::Raw)),
            (b"FORM\0\0\0\0AIFCFVER", Some(Format::Raw)),
            (&id3_flac, Some(Format::Flac)),
            (b"P5 2 1 255 \x00\x01", Some(Format::Raw)),
            (b"P5 2 1 255 \x00", None),
            (b"BM\0\0", None),
            (b"ID3\x04\0\0\0\0\0\x02xx\xff\xfb\x90\x00", Some(Format::Mp3)),
            (b"\xff\xfb\x90\x00", Some(Format::Mp3)),
            (b"\xff\xf3\x48\xc4", Some(Format::Mp3)),
            (b"\xff\xfd\x90\x00", None), // layer II
            (b"\xff\xd8\xff\xe0", Some(Format::Jpeg)),
            (&pe, Some(Format::Pe)),
            (b"MZ\x90\0", None),
            (&macho, Some(Format::MachO)),
            (&macho[..36], None),
            (&fat, Some(Format::MachO)),
            // a Java class file
            (b"\xca\xfe\xba\xbe\0\0\0\x34\0\x1d", None),
            (b"\x1f\x8b\x08\0", Some(Format::Gzip)),
            // a header, then the first block's: fixed, stored, dynamic
            (b"\x78\x9c\x03\x00", Some(Format::Zlib)),
            (b"\x78\xda\x01\x05\x00\xfa\xff", Some(Format::Zlib)),
            (b"\x78\x9d\x03\x00", None),
            (b"\x78\xbb\x03\x00", None), // preset dictionary
            // a header alone, a reserved block type, stored lengths that do not match, more
            // length or distance codes than there are
            (b"\x78\x9c", None),
            (b"\x78\x9c\x07\x00", None),
            (b"\x78\x9c\x01\x05\x00\xfb\xff", None),
            (b"\x78\x9c\xf5\x1c", None),
            (b"\x78\x9c\xed\x1e", None),
            (b"mozLz40\0\x10\0\0\0", Some(Format::Lz4)),
            (b"\x04\x22\x4d\x18\x64\x40", Some(Format::Lz4)),
            (b"mozLz4", None),
            (b"BZh91AY&SY", Some(Format::Bzip2)),
            (b"BZh9\x17\x72\x45\x38\x50\x90", Some(Format::Bzip2)),
            (b"BZh0\x17\x72\x45\x38\x50\x90", None),
            (b"PK\x03\x04\x14\0", Some(Format::Zip)),
            (b"PK\x05\x06", None),
            (b"hello", None),
            (b"", None),
        ];
        for &(d, f) in cases {
            assert_eq!(detect(d), f, "{d:x?}");
        }
        // a stream of dynamic blocks
        let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::new(9));
        std::io::Write::write_all(
            &mut z,
            &std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/src/detect.rs")).unwrap(),
        )
        .unwrap();
        let z = z.finish().unwrap();
        assert_eq!(z[2] >> 1 & 3, 2);
        assert_eq!(detect(&z), Some(Format::Zlib));
        // A database header, and the magic string in SQLite's code: a page size by chance,
        // the rest not what a header must have.
        let mut db = b"SQLite format 3\0".to_vec();
        db.extend_from_slice(&[0x10, 0, 1, 1, 0, 64, 32, 32, 0, 0, 0, 1, 0, 0, 0, 2]);
        assert_eq!(detect(&db), Some(Format::Sqlite));
        let mut code = b"SQLite format 3\0".to_vec();
        code.extend_from_slice(&[0x40, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(detect(&code), None);
        for (at, v) in [(18, 0), (19, 3), (21, 63), (22, 0), (23, 33)] {
            let mut d = db.clone();
            d[at] = v;
            assert_eq!(detect(&d), None, "byte {at}");
        }
    }
}
