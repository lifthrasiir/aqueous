//! Raster images: of 8-bit samples, the pixel model ([`pixel`]) and the coder of raw rasters
//! built on it ([`raster`]); of 16-bit samples, a coder of their own ([`raster16`]).
//!
//! The model predicts a sample by LOCO-I's median edge detector from its left, upper and
//! upper left neighbours, adds the previous channel's residual for the second and third
//! channels (green and blue from red, or their counterparts), and takes its context from the
//! channel and the local activity. aqr-png codes DEFLATE's literals with it (adding the PNG
//! filter's own residuals to the activity); [`raster`] codes the samples of uncompressed
//! images with it (adding the neighbours' residuals).

pub mod pixel;
pub mod raster;
pub mod raster16;
