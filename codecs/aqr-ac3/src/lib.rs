//! AC-3 (ATSC A/52, Dolby Digital) syncframes parsed down to their quantized mantissas and coded again by an adaptive
//! context model, rebuilt bit for bit (aqr-walk walks the syntax once for both directions). Their checksums are derived,
//! not coded. A frame that does not parse, or whose checksums are wrong, is not this crate's: [`check`] says so.
#![no_std]
extern crate alloc;

mod ac3;
mod ac3_tables;

pub use ac3::{Ctx, check, decode, encode, frame_len};
pub use aqr_walk::Error;
pub use aqr_walk::model::Model;
