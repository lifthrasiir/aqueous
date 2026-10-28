//! Clip Studio Paint files (`.clip`): the container ([`clip`]), the tiles its layers are made
//! of ([`tile`]), and a model for tiles of few colours ([`flat`] with adaptive bits, [`fast`]
//! the same in symbols of static tables). Research stage: no formula yet.

pub mod clip;
pub mod fast;
pub mod flat;
pub mod tile;
