//! `pdb-symbols` (aqr-pdb).

#![no_main]

aqr_modules::whole!(inputs [in0: 0, in1: 1, in2: 2, in3: 3], |p, _, i, len| aqr_pdb::module::symbols(p, i, len));
