//! `dwarf-apple` (aqr-dwarf).

#![no_main]

aqr_modules::whole!(inputs [in0: 0, in1: 1, in2: 2, in3: 3, in4: 4], |p, _, i, len| aqr_dwarf::module::apple(p, i, len));
