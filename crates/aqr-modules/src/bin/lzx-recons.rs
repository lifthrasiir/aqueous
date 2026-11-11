//! `lzx-recons` (aqr-lzx), a part module.

#![no_main]

// (its memory laid out as a stack: what it takes is its plan, `aqr_lzx::plan::decode`)
#[global_allocator]
static STACK: aqr_api::stack::Stack = aqr_api::stack::Stack::new();

aqr_api::entry! {
    part fn apply(params_len, in0_len, in1_len, in2_len, in3_len, size)
        windows(
            Input 0 = aqr_lzx::module::WINDOW,
            Input 1 = aqr_lzx::module::WINDOW,
            Input 2 = aqr_lzx::module::WINDOW,
            Input 3 = 1,
            Output 0 = aqr_lzx::module::WINDOW,
        )
    {
        aqr_lzx::module::decode_part(params_len, [in0_len, in1_len, in2_len, in3_len], size)
    }
}
