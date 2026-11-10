//! `jpeg-lossless-recons` (aqr-jpeg), a part module.

#![no_main]

// (its memory laid out as a stack: what it takes is its plan, `aqr_jpeg::lpart::plan`)
#[global_allocator]
static STACK: aqr_api::stack::Stack = aqr_api::stack::Stack::new();

aqr_api::entry! {
    part fn apply(params_len, in0_len, in1_len, in2_len, in3_len, size)
        windows(
            Input 0 = aqr_jpeg::lpart::WINDOW,
            Input 1 = aqr_jpeg::lpart::WINDOW,
            Input 2 = aqr_jpeg::lpart::WINDOW,
            Input 3 = aqr_jpeg::lpart::WINDOW,
            Output 0 = aqr_jpeg::lpart::WINDOW,
        )
    {
        aqr_jpeg::lpart::decode_part(params_len, [in0_len, in1_len, in2_len, in3_len], size)
    }
}
