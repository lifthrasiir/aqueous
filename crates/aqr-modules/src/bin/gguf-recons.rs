//! `gguf-recons` (aqr-gguf), a part module.

#![no_main]

// (its memory laid out as a stack: what it takes is its plan, `aqr_gguf::module::plan`)
#[global_allocator]
static STACK: aqr_api::stack::Stack = aqr_api::stack::Stack::new();

aqr_api::entry! {
    part fn apply(params_len, in0_len, in1_len, in2_len, size)
        windows(
            Input 0 = aqr_gguf::module::WINDOW,
            Input 1 = aqr_gguf::module::WINDOW,
            Input 2 = aqr_gguf::module::WINDOW,
            Output 0 = aqr_gguf::module::WINDOW,
        )
    {
        aqr_gguf::module::decode_part(params_len, [in0_len, in1_len, in2_len], size)
    }
}
