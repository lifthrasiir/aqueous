//! `rar3-recons` (aqr-rar).

#![no_main]

aqr_modules::whole!(inputs [in0: 0, in1: 1, in2: 2], |p, _, i, len| aqr_rar::module::decode3(p, i, len, 1));
