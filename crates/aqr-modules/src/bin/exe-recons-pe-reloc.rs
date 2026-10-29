//! `exe-recons/pe-reloc` (aqr-exe), without std as aqr-exe is.

#![no_std]
#![no_main]

aqr_modules::whole!(inputs [rvas: 0, types: 1], |p, _, i, len| aqr_exe::module::reloc(p, i, len));
