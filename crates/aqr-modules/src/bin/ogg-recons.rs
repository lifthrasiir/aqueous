//! `ogg-recons` (aqr-ogg).

#![no_main]

// (its memory laid out as a stack: what it takes is its plan, `aqr_ogg::plan::decode`)
#[global_allocator]
static STACK: aqr_api::stack::Stack = aqr_api::stack::Stack::new();

aqr_modules::whole!(inputs [in0: 0, in1: 1], |p, _, i, len| aqr_ogg::module::decode(p, i, len));
