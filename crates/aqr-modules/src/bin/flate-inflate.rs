//! `flate-inflate` (aqr-flate).

#![no_main]

aqr_modules::whole!(inputs [in0: 0], |p, _, i, len| aqr_flate::module::inflate(p, i, len));
