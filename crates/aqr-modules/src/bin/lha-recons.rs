//! `lha-recons` (aqr-lha).

#![no_main]

aqr_modules::whole!(inputs [in0: 0, in1: 1, in2: 2], |p, _, i, len| aqr_lha::module::decode(p, i, len, 1));
