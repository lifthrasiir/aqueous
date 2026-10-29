//! `jpeg-recons` (aqr-jpeg).

#![no_main]

// (no threads: rayon runs what it is given on the calling thread; the stripes are tasks, 8 at
// once)
aqr_modules::whole!(slots 8, inputs [in0: 0, in1: 1, in2: 2, in3: 3, in4: 4, in5: 5, in6: 6, in7: 7, in8: 8], |p, _, i, len, _| aqr_jpeg::module::decode(p, i, len, 0));
