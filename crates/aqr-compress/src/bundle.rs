//! Archives that only bundle files: ar, tar and cpio archives (aqr-bundle's formula, the
//! contents of the members a content of the application), asar archives (aqr-asar's, alike),
//! disk images (aqr-disk's, of the file systems of [`FILE_SYSTEMS`], the contents of the
//! files content streams) and game engines' packs (aqr-bundle's `packs`, aqr-disk's formula
//! too), the contents recompressed in turn, each by its kind by its name ([`kind_by_name`]).

use aqr_formula::Formula;

use crate::kind_by_name;

pub(crate) fn bundle(d: &[u8]) -> Option<Formula> {
    aqr_bundle::formula::encode_with(d, &aqr_bundle::formula::Options { kind: kind_by_name, ..Default::default() })
}

/// The formula of game pack `d` of `pack` (aqr-bundle's `packs`: aqr-disk's formula, of the
/// classes of the pack's name).
pub(crate) fn pack(pack: aqr_bundle::packs::Pack, d: &[u8]) -> Option<Formula> {
    aqr_bundle::packs::encode_with(pack, d, kind_by_name)
}

/// The formula of an Unreal 3 package or texture cache `d`: where it is compressed chunks alone
/// (a `.tfc`, or a package's chunk as a content), their LZO1X blocks rebuilt by aqr-lzo (their
/// data a content); else the pack's formula (its chunks, or its exports).
pub(crate) fn unreal_upk(d: &[u8], opt: &crate::Options) -> Option<Formula> {
    let blocks = aqr_bundle::packs::unreal_upk::lzo_blocks(d).map(|b| {
        b.into_iter().map(|(at, len, data)| aqr_lzo::formula::Block { at, len, data: Some(data), stored: false })
    });
    blocks
        .and_then(|b| aqr_lzo::formula::encode_blocks(d, &b.collect::<Vec<_>>(), opt.threads, true))
        .or_else(|| pack(aqr_bundle::packs::Pack::UnrealUpk, d))
}

/// Whether `class` is that of the rest of aqr-disk's formula: of an image, or of a game pack.
fn is_rest(class: &str) -> bool {
    class == aqr_disk::formula::REST
        || aqr_bundle::packs::Pack::ALL.iter().any(|p| aqr_disk::formula::is_rest(class, &[p.name()]))
}

pub(crate) fn asar(d: &[u8]) -> Option<Formula> {
    aqr_asar::formula::encode_with(d, &aqr_asar::formula::Options { kind: kind_by_name, ..Default::default() })
}

/// The file systems of disk images read: ISO 9660 and UDF, FAT, exFAT, NTFS, ext2/3/4, APFS, HFS+,
/// and OLE compound files (Windows Installer packages, Office and HWP documents) as one.
pub(crate) const FILE_SYSTEMS: &[aqr_disk::FileSystem] =
    &[aqr_iso::FS, aqr_fat::FAT, aqr_fat::EXFAT, aqr_ntfs::FS, aqr_ext::FS, aqr_apfs::FS, aqr_hfs::FS, aqr_ole::FS];

pub(crate) fn disk(d: &[u8], threads: usize) -> Option<Formula> {
    aqr_disk::formula::encode_with(
        d,
        &aqr_disk::formula::Options { file_systems: FILE_SYSTEMS, kind: kind_by_name, threads, ..Default::default() },
    )
}

/// Whether formula `f` is one of an archive that only bundles files.
pub(crate) fn bundles(f: &Formula) -> bool {
    f.applies().iter().any(|a| a.module == aqr_decompress::BUNDLE || a.module == aqr_decompress::ASAR) || is_disk(f)
}

/// Whether streams of `class` of an archive that only bundles files ([`bundles`]) are its own
/// (its headers, the rest of an image), not the files'.
pub(crate) fn own_class(class: &str) -> bool {
    class.starts_with("bundle/")
        || is_rest(class)
        || [aqr_asar::formula::HEADER, aqr_asar::formula::INDEX].contains(&class)
}

/// Whether formula `f` is one whose contents are files, each made apart (on a thread of its
/// own): those of an archive that only bundles files ([`bundles`]: ar, tar, cpio, asar, an
/// image), and aqr-wim's.
pub(crate) fn files_apart(f: &Formula) -> bool {
    bundles(f) || f.streams.get(1).is_some_and(|s| s.class == aqr_wim::formula::REST)
}

/// Whether the contents of formula `f` are made apart, each on a thread of its own: files
/// ([`files_apart`]), the entries of a ZIP archive (an APK's DEX files, libraries and
/// resources one after another took most of its time), and the members of a data file of an
/// unknown archive format (The Crew's took 5 s on a thread or two).
pub(crate) fn contents_apart(f: &Formula) -> bool {
    files_apart(f)
        || f.applies().iter().any(|a| a.module == aqr_decompress::ZIP)
        || f.streams.first().is_some_and(|s| s.class == aqr_lzo::formula::MEMBERS)
}

/// What a formula is as its recompressor made it ([`bundles`], [`files_apart`]): told by its
/// streams, which may be gone once its contents are in place.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Kinds {
    pub bundles: bool,
    pub files_apart: bool,
}

impl Kinds {
    pub(crate) fn of(f: &Formula) -> Kinds {
        Kinds { bundles: bundles(f), files_apart: files_apart(f) }
    }
}

/// Whether formula `f` is aqr-disk's, of an image or a game pack.
pub(crate) fn is_disk(f: &Formula) -> bool {
    f.streams.get(1).is_some_and(|s| is_rest(&s.class))
}
