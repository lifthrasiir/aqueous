//! `gif-recons` (aqr-gif).

#![no_main]

aqr_modules::whole!(inputs [in0: 0, in1: 1], |p, _, i, len| aqr_gif::module::decode(p, i, len, 1));
