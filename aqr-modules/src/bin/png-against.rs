//! `png-against` (aqr-png).

#![no_main]

aqr_modules::whole!(inputs [in0: 0, in1: 1, in2: 2, in3: 3], |p, _, i, len| aqr_png::module::decode_against(p, i, len, 1));
