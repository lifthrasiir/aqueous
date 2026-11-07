//! `rar5-recons` (aqr-rar).

#![no_main]

// (its memory laid out as a stack: what it takes is its plan, `aqr_rar::plan::decode5`)
#[global_allocator]
static STACK: aqr_api::stack::Stack = aqr_api::stack::Stack::new();

aqr_modules::whole!(inputs [in0: 0, in1: 1, in2: 2], |p, _, i, len| aqr_rar::module::decode5(p, i, len, 1));
