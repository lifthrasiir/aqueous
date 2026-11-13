//! Instruction set architectures.

pub mod arm64;
pub mod cil;
mod flags;
#[cfg(feature = "encode")]
pub(crate) mod matching;
pub mod x86;
