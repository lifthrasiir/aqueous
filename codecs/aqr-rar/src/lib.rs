//! RAR recompression: archives taken apart into their files' contents (`rar5` reads RAR5
//! archives, `rar4` those of RAR 1.5–4.x), their compressed streams split into the data they
//! decode to and a record of their parse (`recons5`, `recons3`), so that the data is stored
//! instead. `lz5` decodes and encodes RAR5 streams, `lz3` RAR 2.9's (with `ppmd` for their PPMd
//! blocks), `huff` their prefix codes. `FORMAT.md` specifies the records and the modules.

// The code of the modules alone uses only part of the rest.
#![cfg_attr(not(feature = "encode"), allow(unused))]
#[cfg(feature = "encode")]
pub mod formula;
pub mod huff;
pub mod lz3;
pub mod lz5;
pub mod module;
pub mod ppmd;
pub mod ppmd3;
pub mod rar4;
pub mod rar5;
pub mod rec;
pub mod recons3;
pub mod recons5;

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
