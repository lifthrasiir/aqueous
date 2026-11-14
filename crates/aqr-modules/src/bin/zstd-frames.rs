//! `zstd-frames` (aqr-zstd), a part module: the file put out in order, each frame made where its
//! first piece is and held until its last one is put.

#![no_main]

// (its memory laid out as a stack: what it takes is its plan, `aqr_zstd::frames::plan`)
#[global_allocator]
static STACK: aqr_api::stack::Stack = aqr_api::stack::Stack::new();

aqr_api::entry! {
    part fn apply(params_len, in0_len, in1_len, in2_len, in3_len, size)
        windows(
            Input 0 = aqr_zstd::frames::SMALL_WINDOW,
            Input 1 = aqr_zstd::frames::WINDOW,
            Input 2 = aqr_zstd::frames::SMALL_WINDOW,
            Input 3 = aqr_zstd::frames::WINDOW,
            Output 0 = aqr_zstd::frames::WINDOW,
        )
    {
        aqr_zstd::frames::decode_part(params_len, in0_len, in1_len, in2_len, in3_len, size)
    }
}
