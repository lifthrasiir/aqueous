//! `clip-recons` (aqr-clip), the batches of tiles as tasks: given 8 threads (the output is the
//! same whatever the number).

#![no_main]

aqr_modules::whole!(slots 8, inputs [in0: 0, in1: 1, in2: 2, in3: 3], |p, _, i, len, threads| aqr_clip::module::decode(p, i, len, threads));
