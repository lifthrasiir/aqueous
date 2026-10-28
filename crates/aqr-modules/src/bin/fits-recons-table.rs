//! `fits-recons-table` (aqr-fits).

#![no_main]

aqr_modules::whole!(inputs [in0: 0], |p, _, i, len| aqr_fits::module::table(p, i, len));
