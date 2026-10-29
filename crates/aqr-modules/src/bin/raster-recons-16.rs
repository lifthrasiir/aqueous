//! `raster-recons-16` (aqr-raw).

#![no_main]

aqr_modules::whole!(inputs [in0: 0, in1: 1], |p, _, i, len| aqr_raw::module::raster16::decode(p, i, len));
