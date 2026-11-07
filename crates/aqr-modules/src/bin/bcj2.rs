//! `bcj2` (aqr-bcj), a part module of three outputs.

#![no_main]

// (its memory laid out as a stack: what it takes is its plan, `aqr_bcj::formula`'s `memory_part`)
#[global_allocator]
static STACK: aqr_api::stack::Stack = aqr_api::stack::Stack::new();

aqr_api::entry! {
    part fn apply(params_len, in0_len, in1_len, size0, size1, size2)
        windows(
            Input 0 = aqr_bcj::module::BCJ2_WINDOW,
            Input 1 = aqr_bcj::module::BCJ2_WINDOW,
            Output 0 = aqr_bcj::module::BCJ2_WINDOW,
            Output 1 = aqr_bcj::module::BCJ2_WINDOW,
            Output 2 = aqr_bcj::module::BCJ2_WINDOW,
        )
    {
        aqr_bcj::module::bcj2_part(params_len, in0_len, in1_len, size0, size1, size2)
    }
}
