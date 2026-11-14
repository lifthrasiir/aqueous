//! `mca-recons` (aqr-minecraft), a part module: the file put out in order, a chunk at a time.

#![no_main]

// (its memory laid out as a stack: what it takes is its plan, `aqr_minecraft::module::plan`)
#[global_allocator]
static STACK: aqr_api::stack::Stack = aqr_api::stack::Stack::new();

aqr_api::entry! {
    part fn apply(params_len, in0_len, in1_len, in2_len, in3_len, in4_len, size)
        windows(
            Input 0 = aqr_minecraft::module::SMALL_WINDOW,
            Input 1 = aqr_minecraft::module::SMALL_WINDOW,
            Input 2 = aqr_minecraft::module::WINDOW,
            Input 3 = aqr_minecraft::module::WINDOW,
            Input 4 = aqr_minecraft::module::WINDOW,
            Output 0 = aqr_minecraft::module::WINDOW,
        )
    {
        aqr_minecraft::module::decode_part(params_len, in0_len, in1_len, in2_len, in3_len, in4_len, size)
    }
}
