//! `pcm-recons` (aqr-raw).

#![no_main]

aqr_modules::whole!(resources [res0: 0] inputs [in0: 0, in1: 1, in2: 2, in3: 3, in4: 4, in5: 5], |p, r, i, len| aqr_raw::module::pcm::decode(p, r, i, len, 1));
