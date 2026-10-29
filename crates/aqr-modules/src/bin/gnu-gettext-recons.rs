//! `gnu-gettext-recons` (aqr-gnu-gettext).

#![no_main]

aqr_modules::whole!(inputs [in0: 0, in1: 1], |p, _, i, len| aqr_gnu_gettext::module::decode(p, i, len));
