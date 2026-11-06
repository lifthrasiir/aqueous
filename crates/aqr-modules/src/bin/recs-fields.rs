//! `recs-fields` (aqr-infer-recs).

#![no_main]

// (its memory laid out as a stack: what it takes is its plan, `aqr_infer_recs::module`'s)
#[global_allocator]
static STACK: aqr_api::stack::Stack = aqr_api::stack::Stack::new();

aqr_modules::whole!(inputs [planes: 0], |p, _, i, len| aqr_infer_recs::module::decode_fields(p, i, len));
