//! `bcj-arm64` (aqr-bcj).

#![no_main]

aqr_modules::whole!(inputs [in0: 0], |p, _, i, len| aqr_bcj::module::bra(aqr_bcj::bra::Arch::Arm64, p, i, len));
