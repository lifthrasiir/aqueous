//! `lzma-decode` (aqr-lzma).

#![no_main]

// (its memory laid out as a stack: what it takes is its plan, `aqr_lzma::formula`'s `plan_decode`)
#[global_allocator]
static STACK: aqr_api::stack::Stack = aqr_api::stack::Stack::new();

aqr_modules::whole!(inputs [in0: 0], |p, _, i, len| aqr_lzma::module::decode_alone(p, i, len));
