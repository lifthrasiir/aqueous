//! `bcj-powerpc` (aqr-bcj), a part module.

#![no_main]

// (its memory laid out as a stack: what it takes is its plan, `aqr_bcj::formula`'s `memory_part`)
#[global_allocator]
static STACK: aqr_api::stack::Stack = aqr_api::stack::Stack::new();

aqr_api::entry! {
    part fn apply(params_len, in0_len, size)
        windows(Input 0 = aqr_bcj::module::BRA_WINDOW, Output 0 = aqr_bcj::module::BRA_WINDOW)
    {
        aqr_bcj::module::bra_part(aqr_bcj::bra::Arch::PowerPc, params_len, in0_len, size)
    }
}
