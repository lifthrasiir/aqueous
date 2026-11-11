//! Picking a recompressor by the magic bytes at the start of the data, and what data is by
//! the name of its file ([`kind_by_name`]).

use std::fmt;
use std::sync::LazyLock;

use aqr_formula::Kind;

pub use crate::formats::Format;

impl Format {
    /// Whether it only bundles files: an archive of aqr-bundle's or aqr-asar's, or an image of
    /// aqr-disk's.
    pub fn is_bundle(self) -> bool {
        self.spec().bundle
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
    crate::formats::BY_EXT.get(&ext[..]).copied().unwrap_or(Kind::Unknown)
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

/// [`detect`], of data at its start (`start`) or in the middle of other data: the format of
/// the first row of [`ROWS`] that tells it (of those looked for there).
fn detect_at(d: &[u8], start: bool) -> Option<Format> {
    ROWS.iter().find(|r| (start || r.anywhere.is_some()) && r.test.holds(d)).map(|r| r.format)
}

/// How a row of [`ROWS`] tells a file of its format.
enum Test {
    /// These bytes at these offsets, all of them.
    Magic(&'static [(usize, &'static [u8])]),
    /// A function of the data.
    Fn(fn(&[u8]) -> bool),
}

impl Test {
    fn holds(&self, d: &[u8]) -> bool {
        match self {
            Test::Magic(parts) => parts.iter().all(|&(p, m)| d.get(p..p + m.len()) == Some(m)),
            Test::Fn(f) => f(d),
        }
    }
}

/// The first two bytes of the files a row of [`ROWS`] looked for in other data tells: every
/// file it tells starts with one of them ([`starts`] looks at no other row there).
enum Pairs {
    /// Those of its [`Test::Magic`], whose first bytes are at the start.
    Magic,
    Of(&'static [&'static [u8; 2]]),
    Fn(fn(u8, u8) -> bool),
}

/// A way of telling a file of a format by its bytes.
struct Row {
    format: Format,
    test: Test,
    /// Where it is looked for in other data too, the first bytes of the files it tells; else at
    /// the start of data only.
    anywhere: Option<Pairs>,
}

const fn magic(format: Format, parts: &'static [(usize, &'static [u8])]) -> Row {
    Row { format, test: Test::Magic(parts), anywhere: Some(Pairs::Magic) }
}

const fn anywhere(format: Format, pairs: Pairs, f: fn(&[u8]) -> bool) -> Row {
    Row { format, test: Test::Fn(f), anywhere: Some(pairs) }
}

const fn start(format: Format, f: fn(&[u8]) -> bool) -> Row {
    Row { format, test: Test::Fn(f), anywhere: None }
}

/// The ways of telling files by their bytes, in the order they are tried: the first that tells
/// the data gives its format.
#[rustfmt::skip]
static ROWS: &[Row] = {
    use Format::*;
    use Pairs::Of;
    &[
        magic(Png, &[(0, b"\x89PNG\r\n\x1a\n")]),
        anywhere(Gif, Of(&[b"GI"]), aqr_gif::formula::starts),
        start(Aqr, crate::aqueous::starts),
        // (before the others: a hybrid image starts with a boot sector, or anything)
        start(Disk, |d| aqr_disk::starts(d, crate::bundle::FILE_SYSTEMS)),
        // (a compressed container, whose file systems are in its compressed chunks: `chunked`)
        start(Disk, |d| aqr_disk::chunked::container(d).is_some()),
        start(Dmg, crate::chunked::starts),
        start(Wim, aqr_wim::wim::starts),
        start(Asar, aqr_asar::starts),
        start(Chm, aqr_mszip::formula::starts_chm),
        // (a whole file, not in other data)
        start(Sqlite, sqlite_header),
        magic(Pdf, &[(0, b"%PDF-")]),
        magic(Psd, &[(0, b"8BPS")]),
        magic(Clip, &[(0, b"CSFCHUNK")]),
        start(Fsb5, aqr_fsb::fsb5::starts),
        anywhere(Ogg, Of(&[b"Og"]), aqr_ogg::formula::starts),
        magic(Flac, &[(0, b"fLaC")]),
        magic(Raw, &[(0, b"RIFF"), (8, b"WAVE")]),
        magic(Raw, &[(0, b"FORM"), (8, b"AIFF")]),
        magic(Raw, &[(0, b"FORM"), (8, b"AIFC")]),
        // ID3v2 is found before either; what follows the tag tells them apart.
        anywhere(Flac, Of(&[b"ID"]), |d| id3_len(d).is_some_and(|p| d.get(p..p + 4) == Some(b"fLaC"))),
        anywhere(Mp3, Of(&[b"ID"]), |d| id3_len(d).is_some()),
        magic(Jpeg, &[(0, &[0xff, 0xd8, 0xff])]),
        anywhere(Mp3, Pairs::Fn(|a, b| a == 0xff && b & 0xe0 == 0xe0), mp3_header),
        anywhere(Dds, Of(&[b"DD"]), aqr_tex::dds::starts),
        anywhere(Pe, Of(&[b"MZ"]), pe),
        // little-endian (32- or 64-bit), and universal
        anywhere(MachO, Of(&[&[0xce, 0xfa], &[0xcf, 0xfa], &[0xca, 0xfe]]), aqr_exe::format::macho::starts),
        anywhere(Elf, Of(&[&[0x7f, b'E']]), aqr_exe::format::elf::starts),
        anywhere(Lz4, Of(&[b"mo", &[0x04, 0x22]]), aqr_lz4::formula::starts),
        anywhere(Lzo, Of(&[&[0x89, b'L']]), aqr_lzo::formula::starts),
        anywhere(Bvx, Of(&[b"bv"]), aqr_bvx::formula::starts),
        anywhere(Bzip2, Of(&[b"BZ"]), aqr_bz2::formula::starts),
        anywhere(Xz, Of(&[&[0xfd, b'7']]), aqr_lzma::formula::starts_xz),
        magic(Zip, &[(0, b"PK\x03\x04")]),
        anywhere(SevenZip, Of(&[b"7z"]), aqr_7z::formula::starts),
        anywhere(Cab, Of(&[b"MS"]), aqr_mszip::formula::starts),
        anywhere(Alz, Of(&[b"AL"]), aqr_alz::formula::starts),
        anywhere(Egg, Of(&[b"EG"]), aqr_egg::formula::starts),
        anywhere(Rar, Of(&[b"Ra"]), aqr_rar::formula::starts),
        start(GitPack, aqr_git::pack::starts),
        magic(Gzip, &[(0, &[0x1f, 0x8b, 8])]),
        // (a fixed block first only at the start: see `zlib_header`)
        anywhere(Zlib, Pairs::Fn(zlib_pair), |d| zlib_header(d, false)),
        start(Zlib, |d| zlib_header(d, true)),
        // (known by its footer: its end would be that of the data)
        start(LevelDb, aqr_snappy::leveldb::is_table),
        // (before raw: that takes TIFF files of uncompressed samples, and no CR2 or DNG of lossless ones)
        start(JpegLossless, aqr_jpeg::lformula::starts),
        // Two bytes (or DICOM's group 8) are too weak a signature to look for in other data.
        start(Raw, |d| {
            let at = |p: usize, m: &[u8]| d.get(p..p + m.len()) == Some(m);
            ([&b"BM"[..], b"P5", b"P6", b"P7", b"II*\0", b"MM\0*", &[8, 0]].iter().any(|m| at(0, m)) || at(128, b"DICM"))
                && aqr_raw::container::detect(d).is_some()
        }),
        anywhere(Wasm, Of(&[&[0, b'a']]), aqr_wasm::formula::starts),
        start(GnuGettext, aqr_gnu_gettext::formula::starts),
        anywhere(Fits, Of(&[b"SI"]), aqr_fits::formula::starts),
        anywhere(Hdf5, Of(&[&[0x89, b'H']]), aqr_hdf5::formula::starts),
        start(Hdf5, aqr_hdf5::formula::after_user_block),
        start(Dex, aqr_dex::formula::starts),
        start(Pdb, aqr_pdb::formula::starts),
        start(Arsc, aqr_arsc::formula::starts),
        start(Fasta, aqr_fasta::formula::starts),
        start(Gguf, aqr_gguf::formula::starts),
        start(Obj, aqr_obj::formula::starts),
        anywhere(Ar, Of(&[b"!<"]), aqr_bundle::ar::starts),
        // (of the binary forms, only at the start)
        anywhere(Cpio, Of(&[b"07"]), |d| d.starts_with(b"07070") && aqr_bundle::cpio::starts(d)),
        start(Cpio, aqr_bundle::cpio::starts),
        start(Tar, aqr_bundle::tar::starts),
        start(Lzma, aqr_lzma::formula::starts),
        // (four bytes are too weak a signature to look for in other data)
        start(Ppmd, aqr_ppmd::starts),
        start(Mkv, aqr_mkv::starts),
        start(Div3, aqr_div3::formula::starts),
        start(ProRes, aqr_prores::formula::starts),
        // (a header starts with its size and checksum: too weak to look for in other data)
        start(Lha, aqr_lha::formula::starts),
        start(Isc, aqr_isc::formula::starts),
        // (before tables: JSON lines may cut into the same number of fields)
        start(Jsonl, aqr_jsonl::formula::starts),
        start(Xsv, aqr_xsv::formula::starts),
        start(Text, aqr_wrt::formula::starts),
    ]
};

/// zlib's CMF and FLG: DEFLATE with a window of at most 32K, no preset dictionary.
fn zlib_pair(cmf: u8, flg: u8) -> bool {
    cmf & 15 == 8 && cmf >> 4 <= 7 && flg & 0x20 == 0 && (cmf as u16 * 256 + flg as u16).is_multiple_of(31)
}

/// What [`starts`] looks at, made of the rows of [`ROWS`] looked for in other data: the first
/// two bytes of their files, a bit each, and the rows by the first byte.
struct Starts {
    pairs: [u64; 1024],
    rows: [u128; 256],
}

impl Starts {
    fn pair(&self, a: u8, b: u8) -> bool {
        let i = (a as usize) << 8 | b as usize;
        self.pairs[i >> 6] >> (i & 63) & 1 != 0
    }
}

static STARTS: LazyLock<Starts> = LazyLock::new(|| {
    assert!(ROWS.len() <= 128, "rows of a u128");
    let mut t = Starts { pairs: [0; 1024], rows: [0; 256] };
    let mut put = |r: usize, a: u8, b: u8| {
        let i = (a as usize) << 8 | b as usize;
        t.pairs[i >> 6] |= 1 << (i & 63);
        t.rows[a as usize] |= 1 << r;
    };
    for (r, row) in ROWS.iter().enumerate() {
        match (&row.anywhere, &row.test) {
            (None, _) => {}
            (Some(Pairs::Magic), Test::Magic([(0, m), ..])) if m.len() >= 2 => put(r, m[0], m[1]),
            (Some(Pairs::Magic), _) => panic!("row {r} ({}): no first bytes of its magic", row.format),
            (Some(Pairs::Of(pairs)), _) => pairs.iter().for_each(|&&[a, b]| put(r, a, b)),
            (Some(Pairs::Fn(f)), _) => {
                for i in 0..=u16::MAX {
                    let [a, b] = i.to_be_bytes();
                    if f(a, b) {
                        put(r, a, b);
                    }
                }
            }
        }
    }
    t
});

/// Where in `d` from `p` on a file of a format [`starts`] finds may start (by its first two
/// bytes), or its end.
pub(crate) fn next_start(d: &[u8], p: usize) -> usize {
    let t = &*STARTS;
    d.get(p..).and_then(|r| r.windows(2).position(|w| t.pair(w[0], w[1]))).map_or(d.len(), |q| p + q)
}

/// [`detect`] for a file found anywhere in data: by the rows of [`ROWS`] looked for there,
/// only those whose files may start with the first byte (two first bytes no file starts with
/// turned down at once). A zlib stream starting with a fixed block is not taken
/// ([`zlib_header`]).
pub(crate) fn starts(d: &[u8]) -> Option<Format> {
    let t = &*STARTS;
    let (a, b) = (*d.first()?, *d.get(1)?);
    if !t.pair(a, b) {
        return None;
    }
    let mut rows = t.rows[a as usize];
    while rows != 0 {
        let row = &ROWS[rows.trailing_zeros() as usize];
        if row.test.holds(d) {
            return Some(row.format);
        }
        rows &= rows - 1;
    }
    None
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
    if !zlib_pair(cmf, flg) {
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
        let n = (0..65536).filter(|&i| STARTS.pair((i >> 8) as u8, i as u8)).count();
        assert!(n < 400, "{n}");
        for (d, _) in [
            (&b"\x89PNG"[..], 0),
            (b"%PDF", 0),
            (b"8BPS", 0),
            (b"fLaC", 0),
            (b"RIFF", 0),
            (b"Rar!", 0),
            (b"FORM", 0),
            (b"ID3\x04", 0),
            (b"\xff\xd8", 0),
            (b"\xff\xfb", 0),
            (b"MZ", 0),
            (b"\xcf\xfa", 0),
            (b"\xce\xfa", 0),
            (b"\xca\xfe", 0),
            (b"\x7fELF", 0),
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
            assert!(STARTS.pair(d[0], d[1]), "{d:x?}");
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
            assert_eq!(starts(d), detect_at(d, false), "at {p}");
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
        // a 64-bit ELF header of no program or section headers, and a 32-bit one of that
        let mut elf = vec![0u8; 64];
        elf[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
        elf[16] = 3; // ET_DYN
        elf[18] = 62;
        elf[20] = 1;
        elf[52] = 64;
        let mut elf32 = elf.clone();
        elf32[4] = 1;
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
            (&elf, Some(Format::Elf)),
            (&elf[..63], None),
            (&elf32, None), // its header's size that of a 64-bit one
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
