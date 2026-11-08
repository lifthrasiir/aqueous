//! `git-pack-index-recons` (aqr-git), a part module.

#![no_main]

// (its memory laid out as a stack: what it takes is its plan, `aqr_git::module::index_plan`)
#[global_allocator]
static STACK: aqr_api::stack::Stack = aqr_api::stack::Stack::new();

aqr_api::entry! {
    part fn apply(params_len, in0_len, in1_len, in2_len, in3_len, in4_len, size)
        windows(
            Input 0 = aqr_git::module::WINDOW,
            Input 1 = aqr_git::module::SMALL_WINDOW,
            Input 2 = aqr_git::module::SMALL_WINDOW,
            Input 3 = aqr_git::module::WINDOW,
            Input 4 = aqr_git::module::SMALL_WINDOW,
            Output 0 = aqr_git::module::WINDOW,
        )
    {
        aqr_git::module::decode_index_part(params_len, in0_len, in1_len, in2_len, in3_len, in4_len, size)
    }
}
