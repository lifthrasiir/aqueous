//! `exe-recons/arm64-delta` (aqr-exe), without std as aqr-exe is.

#![no_std]
#![no_main]

aqr_modules::whole!(inputs [in0: 0, in1: 1, in2: 2, in3: 3, in4: 4], |p, _, i, len| aqr_exe::module::arm64_delta(p, i, len));
