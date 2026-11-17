//! `exe-recons/pe-reloc` (aqr-exe), without std as aqr-exe is.

#![no_std]
#![no_main]

aqr_modules::whole!(inputs [rvas: 0, types: 1, wrong: 2, pred: 3, data: 4], |p, _, i, len| aqr_exe::module::reloc(p, i, len));
