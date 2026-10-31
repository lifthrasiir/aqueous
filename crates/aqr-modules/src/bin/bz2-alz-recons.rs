//! `bz2-alz-recons` (aqr-bz2).

#![no_main]

aqr_modules::whole!(inputs [in0: 0], |p, _, i, len| aqr_bz2::module::decode_alz(p, i, len, 1));
