//! `b2t-recons` (aqr-b2t), a part module.

#![no_main]

// (its memory laid out as a stack: what it takes is its plan, `aqr_b2t::module::plan`)
#[global_allocator]
static STACK: aqr_api::stack::Stack = aqr_api::stack::Stack::new();

aqr_api::entry! {
    part fn apply(params_len, in0_len, in1_len, in2_len, size)
        windows(
            Input 0 = aqr_b2t::module::WINDOW,
            Input 1 = aqr_b2t::module::WINDOW,
            Input 2 = aqr_b2t::module::WINDOW,
            Output 0 = aqr_b2t::module::WINDOW,
        )
    {
        aqr_b2t::module::decode_part(params_len, in0_len, in1_len, in2_len, size)
    }
}
