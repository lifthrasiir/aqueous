//! `zip-recons` (aqr-zip), a part module.

#![no_main]

aqr_api::entry! {
    part fn apply(params_len, in0_len, in1_len, in2_len, in3_len, size)
        windows(
            Input 0 = aqr_zip::module::WINDOW,
            Input 1 = aqr_zip::module::WINDOW,
            Input 2 = aqr_zip::module::WINDOW,
            Input 3 = aqr_zip::module::WINDOW,
            Output 0 = aqr_zip::module::WINDOW,
        )
    {
        aqr_zip::module::decode_part(params_len, in0_len, in1_len, in2_len, in3_len, size)
    }
}
