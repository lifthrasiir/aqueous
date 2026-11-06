//! `leveldb-recons` (aqr-snappy).

#![no_main]

// (its memory laid out as a stack: what it takes is its plan, `aqr_snappy::module::table_plan`)
#[global_allocator]
static STACK: aqr_api::stack::Stack = aqr_api::stack::Stack::new();

aqr_modules::whole!(inputs [in0: 0, in1: 1, in2: 2], |p, _, i, len| aqr_snappy::module::decode_table(p, i, len, 1));
