//! `lha-recons` (aqr-lha).

#![no_main]

// (its memory laid out as a stack: what it takes is its plan, `aqr_lha::plan::decode`)
#[global_allocator]
static STACK: aqr_api::stack::Stack = aqr_api::stack::Stack::new();

aqr_modules::whole!(inputs [in0: 0, in1: 1, in2: 2], |p, _, i, len| aqr_lha::module::decode(p, i, len, 1));
