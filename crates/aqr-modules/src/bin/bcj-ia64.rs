//! `bcj-ia64` (aqr-bcj).

#![no_main]

// (its memory laid out as a stack: what it takes is its plan, `aqr_bcj::module::plan`)
#[global_allocator]
static STACK: aqr_api::stack::Stack = aqr_api::stack::Stack::new();

aqr_modules::whole!(inputs [in0: 0], |p, _, i, len| aqr_bcj::module::bra(aqr_bcj::bra::Arch::Ia64, p, i, len));
