//! `dex-recons` (aqr-dex).

#![no_main]

// (its memory laid out as a stack: what it takes is its plan, `aqr_dex::module::plan`)
#[global_allocator]
static STACK: aqr_api::stack::Stack = aqr_api::stack::Stack::new();

aqr_modules::whole!(inputs [in0: 0, in1: 1, in2: 2, in3: 3, in4: 4, in5: 5, in6: 6, in7: 7, in8: 8, in9: 9, in10: 10, in11: 11, in12: 12, in13: 13], |p, _, i, len| aqr_dex::module::decode(p, i, len));
