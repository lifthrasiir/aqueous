//! `fits-recons-rice` (aqr-fits).

#![no_main]

// (its memory laid out as a stack: what it takes is its plan, `aqr_fits::module::rice_plan`)
#[global_allocator]
static STACK: aqr_api::stack::Stack = aqr_api::stack::Stack::new();

aqr_modules::whole!(inputs [in0: 0, in1: 1, in2: 2, in3: 3, in4: 4, in5: 5], |p, _, i, len| aqr_fits::module::rice(p, i, len));
