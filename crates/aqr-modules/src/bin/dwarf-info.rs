//! `dwarf-info` (aqr-dwarf).

#![no_main]

// (its memory laid out as a stack: what it takes is its plan, `aqr_dwarf::module::info_plan`)
#[global_allocator]
static STACK: aqr_api::stack::Stack = aqr_api::stack::Stack::new();

aqr_modules::whole!(inputs [in0: 0, in1: 1, in2: 2, in3: 3, in4: 4], |p, _, i, len| aqr_dwarf::module::info(p, i, len));
