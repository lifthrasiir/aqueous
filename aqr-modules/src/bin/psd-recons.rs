//! `psd-recons` (aqr-psd).

#![no_main]

aqr_modules::whole!(inputs [in0: 0, in1: 1, in2: 2, in3: 3], |p, _, i, len| aqr_psd::module::decode(p, i, len, 1));
