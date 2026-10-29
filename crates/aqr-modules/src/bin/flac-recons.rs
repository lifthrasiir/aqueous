//! `flac-recons` (aqr-flac), given 8 threads for its tasks (the output is the same whatever
//! the number).

#![no_main]

aqr_modules::whole!(slots 8, resources [res0: 0] inputs [in0: 0, in1: 1, in2: 2, in3: 3, in4: 4, in5: 5, in6: 6, in7: 7, in8: 8], |p, r, i, len, threads| aqr_flac::module::decode(p, r, i, len, threads));
