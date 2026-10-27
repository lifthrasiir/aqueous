//! `zip-recons` (aqr-zip), given 8 threads for its tasks (the output is the same whatever
//! the number).

#![no_main]

aqr_modules::whole!(resources [res0: 0, res1: 1] inputs [in0: 0, in1: 1, in2: 2, in3: 3], |p, r, i, len| aqr_zip::module::decode(p, r, i, len, 8));
