//! `flac-recons` (aqr-flac).

#![no_main]

aqr_modules::whole!(resources [res0: 0] inputs [in0: 0, in1: 1, in2: 2, in3: 3, in4: 4, in5: 5, in6: 6, in7: 7, in8: 8], |p, r, i, len| aqr_flac::module::decode(p, r, i, len, 1));
