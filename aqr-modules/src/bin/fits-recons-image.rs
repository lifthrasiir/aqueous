//! `fits-recons-image` (aqr-fits).

#![no_main]

aqr_modules::whole!(inputs [in0: 0, in1: 1], |p, _, i, len| aqr_fits::module::image(p, i, len));
