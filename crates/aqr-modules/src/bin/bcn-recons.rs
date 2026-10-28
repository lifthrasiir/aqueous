//! `bcn-recons` (aqr-tex), the stripes of large surfaces as tasks: given 8 threads (the output
//! is the same whatever the number).

#![no_main]

aqr_modules::whole!(inputs [in0: 0, in1: 1], |p, _, i, len| aqr_tex::module::decode(p, i, len, 8));
