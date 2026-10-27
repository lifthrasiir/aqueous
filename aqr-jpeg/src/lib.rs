#![allow(clippy::needless_range_loop)]
// The code of the module alone uses only part of the rest.
#![cfg_attr(not(feature = "encode"), allow(unused))]

pub mod codec;
pub mod fast;
#[cfg(feature = "encode")]
pub mod formula;
pub mod jpeg;
pub mod model;
pub mod module;
pub mod progressive;
pub mod variant;
