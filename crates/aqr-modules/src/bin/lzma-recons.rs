//! `lzma-recons` (aqr-lzma).

#![no_main]

aqr_modules::whole!(inputs [in0: 0, in1: 1, in2: 2], |p, _, i, len| aqr_lzma::module::decode(p, i, len, 1));
