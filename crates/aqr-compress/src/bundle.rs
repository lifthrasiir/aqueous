//! Archives that only bundle files: ar, tar and cpio archives (aqr-bundle's formula, the
//! contents of the members a content of the application) and ISO 9660 and UDF images (aqr-iso's,
//! the contents of the files content streams), the contents recompressed in turn, each by its
//! kind by its name ([`kind_by_name`]).

use aqr_formula::Formula;

use crate::kind_by_name;

pub(crate) fn bundle(d: &[u8]) -> Option<Formula> {
    aqr_bundle::formula::encode_with(d, &aqr_bundle::formula::Options { kind: kind_by_name, ..Default::default() })
}

pub(crate) fn iso(d: &[u8]) -> Option<Formula> {
    aqr_iso::formula::encode_with(d, &aqr_iso::formula::Options { kind: kind_by_name })
}

/// Whether formula `f` is one of an archive that only bundles files.
pub(crate) fn bundles(f: &Formula) -> bool {
    f.applies().iter().any(|a| a.module == aqr_decompress::BUNDLE) || is_iso(f)
}

/// Whether streams of `class` of an archive that only bundles files ([`bundles`]) are its own
/// (its headers, the rest of an image), not the files'.
pub(crate) fn own_class(class: &str) -> bool {
    class.starts_with("bundle/") || class == aqr_iso::formula::REST
}

/// Whether formula `f` is aqr-iso's, of an image.
pub(crate) fn is_iso(f: &Formula) -> bool {
    f.streams.get(1).is_some_and(|s| s.class == aqr_iso::formula::REST)
}
