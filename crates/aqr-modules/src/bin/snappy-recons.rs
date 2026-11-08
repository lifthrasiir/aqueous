//! `snappy-recons` (aqr-snappy), a part module.

#![no_main]

// (its memory laid out as a stack: what it takes is its plan, `aqr_snappy::module::plan`)
#[global_allocator]
static STACK: aqr_api::stack::Stack = aqr_api::stack::Stack::new();

aqr_api::entry! {
    part fn apply(params_len, in0_len, size)
        windows(
            Input 0 = aqr_snappy::module::WINDOW,
            Output 0 = aqr_snappy::module::WINDOW,
        )
    {
        aqr_snappy::module::decode_part(params_len, in0_len, size)
    }
}
