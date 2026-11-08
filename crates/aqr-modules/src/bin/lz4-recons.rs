//! `lz4-recons` (aqr-lz4), a part module.

#![no_main]

// (its memory laid out as a stack: what it takes is its plan, `aqr_lz4::part::plan`)
#[global_allocator]
static STACK: aqr_api::stack::Stack = aqr_api::stack::Stack::new();

aqr_api::entry! {
    part fn apply(params_len, in0_len, size)
        windows(
            Input 0 = aqr_lz4::part::WINDOW,
            Output 0 = aqr_lz4::part::WINDOW,
        )
    {
        aqr_lz4::part::decode_part(params_len, in0_len, size)
    }
}
