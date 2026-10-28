//! `fits-recons-rice` (aqr-fits).

#![no_main]

aqr_modules::whole!(inputs [in0: 0, in1: 1, in2: 2, in3: 3, in4: 4, in5: 5], |p, _, i, len| aqr_fits::module::rice(p, i, len));
