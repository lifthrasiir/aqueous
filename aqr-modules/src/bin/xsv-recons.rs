//! `xsv-recons` (aqr-xsv).

#![no_main]

aqr_modules::whole!(inputs [in0: 0, in1: 1, in2: 2, in3: 3, in4: 4, in5: 5, in6: 6], |p, _, i, len| aqr_xsv::module::decode(p, i, len, 1));
