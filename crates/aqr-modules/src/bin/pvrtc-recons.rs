//! `pvrtc-recons` (aqr-tex): a surface at a time, of no stripes (its words depend on those around
//! them).

#![no_main]

// (its memory laid out as a stack: what it takes is its plan, `aqr_tex::module::plan_of`)
#[global_allocator]
static STACK: aqr_api::stack::Stack = aqr_api::stack::Stack::new();

aqr_modules::whole!(inputs [in0: 0, in1: 1], |p, _, i, len| aqr_tex::module::decode_pvrtc(p, i, len, 1));
