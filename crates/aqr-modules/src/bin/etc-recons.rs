//! `etc-recons` (aqr-tex), the stripes of large surfaces as tasks: given 8 threads (the output
//! is the same whatever the number).

#![no_main]

// (its memory laid out as a stack: what it takes is its plan, `aqr_tex::module::plan_etc`)
#[global_allocator]
static STACK: aqr_api::stack::Stack = aqr_api::stack::Stack::new();

aqr_modules::whole!(slots 8, inputs [in0: 0, in1: 1], |p, _, i, len, threads| aqr_tex::module::decode_etc(p, i, len, threads));
