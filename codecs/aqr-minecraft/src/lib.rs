//! Minecraft's region files (`.mca`, of chunks, entities and points of interest): a file is
//! rebuilt bit for bit from the data of its chunks, their zlib streams made again by aqr-flate's
//! emulation of zlib, so that the archive stores the data instead. `formula` is the entry point
//! (see aqr-formula); `FORMAT.md` specifies the module; `region` reads the files.

#[cfg(feature = "encode")]
pub mod formula;
pub mod layout;
pub mod module;
pub mod region;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error(pub &'static str);

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for Error {}

impl From<aqr_common::Corrupt> for Error {
    fn from(_: aqr_common::Corrupt) -> Self {
        corrupt()
    }
}

impl From<aqr_api::stream::Corrupt> for Error {
    fn from(_: aqr_api::stream::Corrupt) -> Self {
        corrupt()
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[cold]
pub fn corrupt() -> Error {
    Error("corrupt input")
}
