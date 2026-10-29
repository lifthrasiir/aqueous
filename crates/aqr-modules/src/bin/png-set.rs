//! `png-set` (aqr-png), given 8 threads for its tasks (the output is the same whatever
//! the number).

#![no_main]

aqr_modules::whole!(slots 8, inputs [in0: 0, in1: 1, in2: 2], |p, _, i, len, threads| aqr_png::module::decode_set(p, i, len, threads));
