//! `fits-recons-table` (aqr-fits).

#![no_main]

// (its memory laid out as a stack: what it takes is its plan, `aqr_fits::module::table_plan`)
#[global_allocator]
static STACK: aqr_api::stack::Stack = aqr_api::stack::Stack::new();

aqr_modules::whole!(inputs [in0: 0], |p, _, i, len| aqr_fits::module::table(p, i, len));
