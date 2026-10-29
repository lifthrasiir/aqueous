//! `vorbis-recons` (aqr-vorbis).

#![no_main]

aqr_modules::whole!(inputs [in0: 0, in1: 1], |p, _, i, len| aqr_vorbis::module::decode(p, i, len));
