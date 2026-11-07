//! Clip Studio Paint files (`.clip`): the container ([`clip`]), the tiles its layers are made
//! of ([`tile`]), a model of tiles as whole pixels ([`dynamic`] with adaptive bits; `static`,
//! research, the same in symbols of static tables), the zlib streams made again ([`zlib`]), and
//! the module `clip-recons` ([`module`], `formula`; FORMAT.md).

pub mod clip;
pub mod dynamic;
#[cfg(feature = "encode")]
pub mod formula;
pub mod module;
#[cfg(feature = "encode")]
pub mod plan;
// (a keyword: `aqr_clip::r#static`)
#[cfg(feature = "encode")]
pub mod r#static;
pub mod tile;
pub mod zlib;
