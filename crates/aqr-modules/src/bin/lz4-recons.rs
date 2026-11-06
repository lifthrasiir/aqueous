//! `lz4-recons` (aqr-lz4).

#![no_main]

// (its memory laid out as a stack: what it takes is its plan, `aqr_lz4::module::plan`)
#[global_allocator]
static STACK: aqr_api::stack::Stack = aqr_api::stack::Stack::new();

aqr_modules::whole!(inputs [in0: 0], |p, _, i, len| aqr_lz4::module::decode(p, i, len, 1));
