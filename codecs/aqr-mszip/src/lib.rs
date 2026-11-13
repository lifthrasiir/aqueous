//! CAB files (MS-CAB) and CHM files taken apart into the contents of their files, so that those
//! are stored instead: an MSZIP folder is rebuilt from its data by aqr-flate's module and this
//! crate's (`mszip-recons`), an LZX folder (or a CHM's LZX section) by aqr-lzx's, a stored folder
//! is its data. `mszip` turns a folder's MSZIP blocks into one DEFLATE stream and back; `cab` and
//! `chm` read the containers; `formula` is the entry point (see aqr-formula), with the feature
//! `encode`; `module` is the code of the module, which `FORMAT.md` specifies.

#[cfg(feature = "encode")]
pub mod cab;
#[cfg(feature = "encode")]
pub mod chm;
#[cfg(feature = "encode")]
pub mod formula;
pub mod module;
pub mod mszip;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error(pub &'static str);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

#[cold]
pub fn corrupt() -> Error {
    Error("corrupt input")
}
