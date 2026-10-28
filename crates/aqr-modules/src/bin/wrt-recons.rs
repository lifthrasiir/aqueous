//! `wrt-recons` (aqr-wrt).

#![no_main]

aqr_modules::whole!(resources [res0: 0] inputs [in0: 0], |p, r, i, len| aqr_wrt::module::decode(p, r, i, len, 1));
