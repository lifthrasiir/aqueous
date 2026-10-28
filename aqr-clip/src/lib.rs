//! Clip Studio Paint files (`.clip`): the container ([`clip`]), the tiles its layers are made
//! of ([`tile`]), and a model of tiles as whole pixels: [`dynamic`] with adaptive bits, `static`
//! the same in symbols of static tables. Research stage: no formula yet.

pub mod clip;
pub mod dynamic;
// (a keyword: `aqr_clip::r#static`)
pub mod r#static;
pub mod tile;
