//! `bz2-recons` (aqr-bz2).

#![no_main]

aqr_modules::whole!(inputs [in0: 0], |p, _, i, len| aqr_bz2::module::decode(p, i, len, 1));
