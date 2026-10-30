//! `bcj-delta` (aqr-bcj).

#![no_main]

aqr_modules::whole!(inputs [in0: 0], |p, _, i, len| aqr_bcj::module::delta(p, i, len));
