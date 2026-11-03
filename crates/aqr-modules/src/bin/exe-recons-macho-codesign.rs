//! `exe-recons/macho-codesign` (aqr-exe), without std as aqr-exe is.

#![no_std]
#![no_main]

aqr_modules::whole!(inputs [in0: 0, in1: 1], |p, _, i, len| aqr_exe::module::macho_codesign(p, i, len));
