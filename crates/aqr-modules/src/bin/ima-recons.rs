//! `ima-recons` (aqr-ima).

#![no_main]

aqr_modules::whole!(inputs [in0: 0], |p, _, i, len| aqr_ima::module::decode(p, i, len));
