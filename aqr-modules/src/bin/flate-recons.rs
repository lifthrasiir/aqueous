//! `flate-recons` (aqr-flate).

#![no_main]

aqr_modules::whole!(resources [res0: 0, res1: 1] inputs [in0: 0, in1: 1], |p, r, i, len| aqr_flate::module::decode(p, r, i, len, 1));
