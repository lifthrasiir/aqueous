//! `snappy-recons` (aqr-snappy).

#![no_main]

aqr_modules::whole!(inputs [in0: 0], |p, _, i, len| aqr_snappy::module::decode(p, i, len, 1));
