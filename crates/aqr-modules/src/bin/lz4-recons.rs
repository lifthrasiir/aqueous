//! `lz4-recons` (aqr-lz4).

#![no_main]

aqr_modules::whole!(inputs [in0: 0], |p, _, i, len| aqr_lz4::module::decode(p, i, len, 1));
