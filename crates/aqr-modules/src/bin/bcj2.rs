//! `bcj2` (aqr-bcj).

#![no_main]

aqr_modules::whole!(inputs [in0: 0, in1: 1], |p, _, i, len| aqr_bcj::module::bcj2(p, i, len));
