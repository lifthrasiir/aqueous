//! `jpeg-lossless-recons` (aqr-jpeg).

#![no_main]

// (its memory laid out as a stack: what it takes is its plan, `aqr_jpeg::lmodule::plan`)
#[global_allocator]
static STACK: aqr_api::stack::Stack = aqr_api::stack::Stack::new();

// (no threads: rayon runs what it is given on the calling thread)
aqr_modules::whole!(
    inputs [in0: 0, in1: 1, in2: 2, in3: 3],
    |p, _, i, len| aqr_jpeg::lmodule::decode(p, i, len, 0)
);
