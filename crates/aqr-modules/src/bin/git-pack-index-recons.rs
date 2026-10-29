//! `git-pack-index-recons` (aqr-git).

#![no_main]

aqr_modules::whole!(inputs [in0: 0, in1: 1, in2: 2, in3: 3], |p, _, i, len| aqr_git::module::decode_index(p, i, len, 1));
