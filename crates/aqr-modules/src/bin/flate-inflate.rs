//! `flate-inflate` (aqr-flate), a part module.

#![no_main]

aqr_api::entry! {
    part fn apply(params_len, in0_len, size)
        windows(Input 0 = aqr_flate::module::INFLATE_WINDOW, Output 0 = aqr_flate::module::INFLATE_WINDOW)
    {
        aqr_flate::module::inflate_part(params_len, in0_len, size)
    }
}
