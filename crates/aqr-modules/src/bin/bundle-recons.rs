//! `bundle-recons` (aqr-bundle).

#![no_main]

// (its memory laid out as a stack: what it takes is its plan, `aqr_bundle::module::plan`)
#[global_allocator]
static STACK: aqr_api::stack::Stack = aqr_api::stack::Stack::new();

aqr_modules::whole!(inputs [in0: 0, in1: 1], |p, _, i, len| aqr_bundle::module::decode(p, i, len, 1));
