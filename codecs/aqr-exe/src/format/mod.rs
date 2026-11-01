//! Executable file formats.

#[cfg(feature = "encode")]
pub mod elf;
#[cfg(feature = "encode")]
pub mod macho;
#[cfg(feature = "encode")]
pub mod pe;
