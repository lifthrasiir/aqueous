//! `png-against` (aqr-png), given 8 threads for its tasks (the output is the same whatever
//! the number).

#![no_main]

// (its memory laid out as a stack: what it takes is its plan, `aqr_png::plan::against`)
#[global_allocator]
static STACK: aqr_api::stack::Stack = aqr_api::stack::Stack::new();

aqr_modules::whole!(slots 8, inputs [in0: 0, in1: 1, in2: 2, in3: 3], |p, _, i, len, threads| aqr_png::module::decode_against(p, i, len, threads));
