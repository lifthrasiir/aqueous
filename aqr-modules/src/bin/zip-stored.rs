//! `zip-stored` (aqr-zip).

#![no_main]

aqr_modules::whole!(inputs [in0: 0, in1: 1, in2: 2, in3: 3], |p, _, i, len| aqr_zip::module::decode_stored(p, i, len, 1));
