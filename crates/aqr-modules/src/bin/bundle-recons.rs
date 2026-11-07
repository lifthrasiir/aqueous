//! `bundle-recons` (aqr-bundle), a part module.

#![no_main]

// (its memory laid out as a stack: what it takes is its plan, `aqr_bundle::module::plan`)
#[global_allocator]
static STACK: aqr_api::stack::Stack = aqr_api::stack::Stack::new();

aqr_api::entry! {
    part fn apply(params_len, in0_len, in1_len, size)
        windows(
            Input 0 = aqr_bundle::module::WINDOW,
            Input 1 = aqr_bundle::module::WINDOW,
            Output 0 = aqr_bundle::module::WINDOW,
        )
    {
        aqr_bundle::module::decode_part(params_len, in0_len, in1_len, size)
    }
}
