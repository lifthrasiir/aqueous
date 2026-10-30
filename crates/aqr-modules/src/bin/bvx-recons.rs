//! `bvx-recons` (aqr-bvx).

#![no_main]

aqr_modules::whole!(inputs [in0: 0, in1: 1], |p, _, i, len| aqr_bvx::module::decode(p, i, len, 1));
