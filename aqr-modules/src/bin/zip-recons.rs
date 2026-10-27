//! `zip-recons` (aqr-zip).

#![no_main]

aqr_modules::whole!(inputs [in0: 0, in1: 1, in2: 2, in3: 3], |p, _, i, len| aqr_zip::module::decode(p, i, len, 1));
