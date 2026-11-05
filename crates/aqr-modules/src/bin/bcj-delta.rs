//! `bcj-delta` (aqr-bcj), a part module.

#![no_main]

aqr_api::entry! {
    part fn apply(params_len, in0_len, size)
        windows(Input 0 = aqr_bcj::module::DELTA_WINDOW, Output 0 = aqr_bcj::module::DELTA_WINDOW)
    {
        aqr_bcj::module::delta_part(params_len, in0_len, size)
    }
}
