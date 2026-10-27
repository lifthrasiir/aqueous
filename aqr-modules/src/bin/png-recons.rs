//! `png-recons` (aqr-png), given 8 threads for its tasks (the output is the same whatever
//! the number).

#![no_main]

aqr_modules::whole!(inputs [in0: 0, in1: 1, in2: 2], |p, _, i, len| aqr_png::module::decode(p, i, len, 8));
