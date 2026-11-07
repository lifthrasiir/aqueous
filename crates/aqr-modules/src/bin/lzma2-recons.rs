//! `lzma2-recons` (aqr-lzma), a part module.

#![no_main]

// (its memory laid out as a stack: what it takes is its plan, `aqr_lzma::plan::decode2`)
#[global_allocator]
static STACK: aqr_api::stack::Stack = aqr_api::stack::Stack::new();

aqr_api::entry! {
    part fn apply(params_len, in0_len, in1_len, in2_len, size)
        windows(
            Input 0 = aqr_lzma::module::WINDOW,
            Input 1 = aqr_lzma::module::WINDOW,
            Input 2 = aqr_lzma::module::WINDOW,
            Output 0 = aqr_lzma::module::WINDOW,
        )
    {
        aqr_lzma::module::decode2_part(params_len, [in0_len, in1_len, in2_len], size)
    }
}
