//! `bz2-recons` (aqr-bz2), a part module.

#![no_main]

// (its memory laid out as a stack: what it takes is its plan, `aqr_bz2::module::plan`)
#[global_allocator]
static STACK: aqr_api::stack::Stack = aqr_api::stack::Stack::new();

aqr_api::entry! {
    part fn apply(params_len, in0_len, size)
        windows(
            Input 0 = aqr_bz2::module::WINDOW,
            Output 0 = aqr_bz2::module::WINDOW,
        )
    {
        aqr_bz2::module::decode_part(params_len, in0_len, size)
    }
}
