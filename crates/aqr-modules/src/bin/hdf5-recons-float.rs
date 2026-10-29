//! `hdf5-recons-float` (aqr-hdf5).

#![no_main]

aqr_modules::whole!(inputs [in0: 0, in1: 1, in2: 2], |p, _, i, len| aqr_hdf5::module::float(p, i, len));
