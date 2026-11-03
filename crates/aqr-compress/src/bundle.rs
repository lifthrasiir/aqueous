//! Archives that only bundle files: ar, tar and cpio archives (aqr-bundle's formula, the
//! contents of the members a content of the application), asar archives (aqr-asar's, alike)
//! and ISO 9660 and UDF images (aqr-iso's, the contents of the files content streams), the
//! contents recompressed in turn, each by its kind by its name ([`kind_by_name`]).

use aqr_formula::Formula;

use crate::kind_by_name;

pub(crate) fn bundle(d: &[u8]) -> Option<Formula> {
    aqr_bundle::formula::encode_with(d, &aqr_bundle::formula::Options { kind: kind_by_name, ..Default::default() })
}

pub(crate) fn asar(d: &[u8]) -> Option<Formula> {
    aqr_asar::formula::encode_with(d, &aqr_asar::formula::Options { kind: kind_by_name, ..Default::default() })
}

pub(crate) fn iso(d: &[u8]) -> Option<Formula> {
    aqr_iso::formula::encode_with(d, &aqr_iso::formula::Options { kind: kind_by_name })
}

/// Whether formula `f` is one of an archive that only bundles files.
pub(crate) fn bundles(f: &Formula) -> bool {
    f.applies().iter().any(|a| a.module == aqr_decompress::BUNDLE || a.module == aqr_decompress::ASAR) || is_iso(f)
}

/// Whether streams of `class` of an archive that only bundles files ([`bundles`]) are its own
/// (its headers, the rest of an image), not the files'.
pub(crate) fn own_class(class: &str) -> bool {
    class.starts_with("bundle/")
        || [aqr_iso::formula::REST, aqr_asar::formula::HEADER, aqr_asar::formula::INDEX].contains(&class)
}

/// Whether formula `f` is one whose contents are files, each made apart (on a thread of its
/// own): those of an archive that only bundles files ([`bundles`]: ar, tar, cpio, asar, an
/// image), and aqr-wim's.
pub(crate) fn files_apart(f: &Formula) -> bool {
    bundles(f) || f.streams.get(1).is_some_and(|s| s.class == aqr_wim::formula::REST)
}

/// Whether the contents of formula `f` are made apart, each on a thread of its own: files
/// ([`files_apart`]), and the entries of a ZIP archive (an APK's DEX files, libraries and
/// resources one after another took most of its time).
pub(crate) fn contents_apart(f: &Formula) -> bool {
    files_apart(f) || f.applies().iter().any(|a| a.module == aqr_decompress::ZIP)
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

/// Whether formula `f` is aqr-iso's, of an image.
pub(crate) fn is_iso(f: &Formula) -> bool {
    f.streams.get(1).is_some_and(|s| s.class == aqr_iso::formula::REST)
}
