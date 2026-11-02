//! `pdb-msf` (aqr-pdb).

#![no_main]

aqr_modules::whole!(inputs [in0: 0, in1: 1, in2: 2], |p, _, i, len| aqr_pdb::module::msf(p, i, len));
