//! `bz2-alz-recons` (aqr-bz2).

#![no_main]

// (its memory laid out as a stack: what it takes is its plan, `aqr_bz2::module::plan`)
#[global_allocator]
static STACK: aqr_api::stack::Stack = aqr_api::stack::Stack::new();

aqr_modules::whole!(inputs [in0: 0], |p, _, i, len| aqr_bz2::module::decode_alz(p, i, len, 1));
