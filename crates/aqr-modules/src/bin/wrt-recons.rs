//! `wrt-recons` (aqr-wrt).

#![no_main]

// (its memory laid out as a stack: what it takes is its plan, `aqr_wrt::plan::recons`)
#[global_allocator]
static STACK: aqr_api::stack::Stack = aqr_api::stack::Stack::new();

aqr_modules::whole!(resources [res0: 0] inputs [in0: 0], |p, r, i, len| aqr_wrt::module::decode(p, r, i, len, 1));
