//! AC-3 (ATSC A/52, Dolby Digital) syncframes parsed down to their quantized mantissas and coded again by an adaptive
//! context model, rebuilt bit for bit (aqr-walk walks the syntax once for both directions). Their checksums are derived,
//! not coded. A byte stream is syncframes in a row and whatever else lies between them (`stream`): the frames are coded,
//! the rest kept as it is. `module` is the code of the module `ac3-recons`; `formula` (feature `encode`) makes the
//! formulas that use it.
#![no_std]
extern crate alloc;
#[cfg(feature = "encode")]
extern crate std;

mod ac3;
mod ac3_tables;
#[cfg(feature = "encode")]
pub mod formula;
pub mod module;
/// The module natively on whole inputs (not in the module: what aqr-decompress and aqr-dll run).
#[cfg(not(target_family = "wasm"))]
pub mod native;
pub mod stream;

pub use ac3::{BLK_BYTES, Ctx, check, decode, encode, frame_len};
pub use aqr_walk::Error;
pub use aqr_walk::model::Model;

/// The error of the module and the formula.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleError(pub &'static str);

impl core::fmt::Display for ModuleError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.0)
    }
}

#[cfg(feature = "encode")]
impl std::error::Error for ModuleError {}

pub type Result<T> = core::result::Result<T, ModuleError>;

#[cold]
pub fn corrupt() -> ModuleError {
    ModuleError("corrupt input")
}
