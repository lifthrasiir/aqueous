//! `jpeg-recons` (aqr-jpeg).

#![no_main]

// (no threads: rayon runs what it is given on the calling thread)
aqr_modules::whole!(inputs [in0: 0, in1: 1, in2: 2, in3: 3, in4: 4, in5: 5, in6: 6, in7: 7, in8: 8], |p, _, i, len| aqr_jpeg::module::decode(p, i, len, 0));
