//! `exe-recons/arm64` (aqr-exe), without std as aqr-exe is.

#![no_std]
#![no_main]

aqr_modules::whole!(inputs [in0: 0, in1: 1, in2: 2, in3: 3, in4: 4, in5: 5, in6: 6, in7: 7, in8: 8, in9: 9, in10: 10], |p, _, i, len| aqr_exe::module::arm64(p, i, len));
