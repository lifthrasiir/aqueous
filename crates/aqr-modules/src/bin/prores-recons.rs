//! `prores-recons` (aqr-prores).

#![no_main]

aqr_modules::whole!(slots 8, inputs [in0: 0, in1: 1], |p, _, i, len, threads| aqr_prores::module::decode(p, i, len, threads));
