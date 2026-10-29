//! `pcm-recons` (aqr-raw), given 8 threads for its tasks (the output is the same whatever
//! the number).

#![no_main]

aqr_modules::whole!(slots 8, resources [res0: 0] inputs [in0: 0, in1: 1, in2: 2, in3: 3, in4: 4, in5: 5], |p, r, i, len, threads| aqr_raw::module::pcm::decode(p, r, i, len, threads));
