//! `exe-recons/macho-codesign` (aqr-exe), a part module, without std as aqr-exe is.

#![no_std]
#![no_main]

aqr_api::entry! {
    part fn apply(params_len, in0_len, in1_len, size)
        windows(
            Input 0 = aqr_exe::module::MACHO_CODESIGN_WINDOW,
            Input 1 = aqr_exe::module::MACHO_CODESIGN_WINDOW,
            Output 0 = aqr_exe::module::MACHO_CODESIGN_WINDOW,
        )
    {
        aqr_exe::module::macho_codesign_part(params_len, [in0_len, in1_len], size)
    }
}
