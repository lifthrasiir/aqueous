//! `git-pack-index-recons` (aqr-git).

#![no_main]

// (its memory laid out as a stack: what it takes is its plan, `aqr_git::module::index_plan`)
#[global_allocator]
static STACK: aqr_api::stack::Stack = aqr_api::stack::Stack::new();

aqr_modules::whole!(inputs [in0: 0, in1: 1, in2: 2, in3: 3], |p, _, i, len| aqr_git::module::decode_index(p, i, len, 1));
