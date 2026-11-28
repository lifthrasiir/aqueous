//! What aqr-win and its shell extension (aqr-win-shell) agree on of Explorer's verbs: the verbs
//! taking many paths and the classes that take them as a drop, and the archives' extensions.

/// The verbs taking many paths, each a class of aqr-win's of its own (`aqr-win --drop VERB`, a
/// COM local server: its `drop`), on which the paths chosen are dropped at once: its CLSID is
/// [`DROP_BASE`] with its index in the last byte. Never reorder (the CLSIDs registered would go
/// to other verbs); add at the end.
pub const DROP_VERBS: [&str; 8] = [
    "compress",
    "compress-passphrase",
    "compress-zip",
    "compress-7z",
    "compress-options",
    "extract",
    "test",
    "extract-to",
];

/// The CLSID of the verbs' classes, the last byte the verb's index.
pub const DROP_BASE: u128 = 0x88c8460e_ca88_454d_b38c_3b192ee4e700;

/// The CLSID of `verb`'s class (as a number), where it is one of [`DROP_VERBS`].
pub fn drop_class(verb: &str) -> Option<u128> {
    DROP_VERBS.iter().position(|&v| v == verb).map(|i| DROP_BASE | i as u128)
}

/// The extensions of the archives Aqueous opens (its own first), as Explorer tells a file's type
/// by its last: aqr-win registers a class of each.
pub const ARCHIVES: [&str; 15] =
    ["aqr", "zip", "7z", "rar", "alz", "egg", "tar", "tgz", "tbz2", "txz", "gz", "bz2", "xz", "lzma", "zst"];

/// Whether `path` is of an archive by its extension ([`ARCHIVES`], in any case).
pub fn is_archive(path: &std::path::Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| ARCHIVES.iter().any(|a| a.eq_ignore_ascii_case(e)))
}

/// Whether `path` is of an Aqueous archive (`.aqr`, in any case): of journal points.
pub fn is_aqueous(path: &std::path::Path) -> bool {
    path.extension().is_some_and(|e| e.eq_ignore_ascii_case("aqr"))
}
