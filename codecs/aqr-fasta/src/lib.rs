//! FASTA files of nucleotide sequences, rebuilt bit for bit from their parts: the bases A, C,
//! G and T as symbols coded by an adaptive order-9 model and rANS ([`bases`]), and apart from
//! them the headers, the line layout, the case and the other letters (N runs, IUPAC codes)
//! ([`fasta`]). `formula` is the entry point (a module of aqr-formula); `FORMAT.md`
//! specifies it.

pub mod bases;
pub mod fasta;
#[cfg(feature = "encode")]
pub mod formula;
pub mod module;

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
