//! `mszip-recons` (aqr-mszip).

#![no_main]

// (its memory laid out as a stack: what it takes is its plan, `aqr_mszip::module::plan`)
#[global_allocator]
static STACK: aqr_api::stack::Stack = aqr_api::stack::Stack::new();

aqr_modules::whole!(inputs [in0: 0, in1: 1, in2: 2, in3: 3], |p, _, i, len| aqr_mszip::module::decode(p, i, len));
