//! Electron's asar archives: a header of JSON telling the files, then their contents. An
//! archive is rebuilt bit for bit from the bytes after its header, its *data*, and the header
//! with what follows from the data left out: the files' sizes and offsets and their SHA-256
//! integrity, the whole file's and its blocks'. The contents are slices of the data, each to
//! recompress, deduplicate or compress with its like. `formula` is the entry point (see
//! aqr-formula); `FORMAT.md` specifies the module.
//!
//! `layout` rebuilds the archive (the module's code); `json` reads the header into the fields
//! the encoder derives (encoder only).

// The code of the modules alone uses only part of the rest.
#![cfg_attr(not(feature = "encode"), allow(unused))]
#[cfg(feature = "encode")]
pub mod formula;
#[cfg(feature = "encode")]
pub mod json;
pub mod layout;
pub mod module;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error(pub &'static str);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for Error {}

impl From<aqr_common::Corrupt> for Error {
    fn from(_: aqr_common::Corrupt) -> Self {
        corrupt()
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[cold]
pub fn corrupt() -> Error {
    Error("corrupt input")
}

/// The header of the asar archive at the start of `d`: the JSON's span, and where the data
/// starts. Its pickles are taken as they are (`4`, then the size of the second, its payload's
/// size, then the JSON's length), as far as they fit `d`, the JSON starting `{"files":`.
pub fn header(d: &[u8]) -> Option<(std::ops::Range<usize>, usize)> {
    let u32le = |p: usize| Some(u32::from_le_bytes(*d.get(p..)?.first_chunk()?) as usize);
    if u32le(0)? != 4 {
        return None;
    }
    let (start, json) = (8 + u32le(4)?, 16..16 + u32le(12)?);
    (json.end <= start && start <= d.len() && d[json.clone()].starts_with(b"{\"files\":")).then_some((json, start))
}

/// Whether `d` starts like an asar archive ([`header`]).
pub fn starts(d: &[u8]) -> bool {
    header(d).is_some()
}
