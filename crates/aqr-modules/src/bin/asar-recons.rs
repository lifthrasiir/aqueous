//! `asar-recons` (aqr-asar).

#![no_main]

// (its memory laid out as a stack: what it takes is its plan, `aqr_asar::module::plan`)
#[global_allocator]
static STACK: aqr_api::stack::Stack = aqr_api::stack::Stack::new();

aqr_modules::whole!(inputs [in0: 0, in1: 1, in2: 2], |p, _, i, len| aqr_asar::module::decode(p, i, len, 1));
