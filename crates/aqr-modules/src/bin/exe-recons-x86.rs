//! `exe-recons/x86` (aqr-exe), a part module, without std as aqr-exe is.

#![no_std]
#![no_main]

aqr_api::entry! {
    part fn apply(params_len, res0_len, in0_len, in1_len, in2_len, in3_len, in4_len, in5_len, in6_len, in7_len, in8_len, in9_len, in10_len, in11_len, in12_len, size)
        windows(
            Input 0 = aqr_exe::module::X86_WINDOW,
            Input 1 = aqr_exe::module::X86_WINDOW,
            Input 2 = aqr_exe::module::X86_WINDOW,
            Input 3 = aqr_exe::module::X86_WINDOW,
            Input 4 = aqr_exe::module::X86_WINDOW,
            Input 5 = aqr_exe::module::X86_WINDOW,
            Input 6 = aqr_exe::module::X86_WINDOW,
            Input 7 = aqr_exe::module::X86_WINDOW,
            Input 8 = aqr_exe::module::X86_WINDOW,
            Input 9 = aqr_exe::module::X86_WINDOW,
            Input 10 = aqr_exe::module::X86_WINDOW,
            Input 11 = aqr_exe::module::X86_WINDOW,
            Input 12 = aqr_exe::module::X86_WINDOW,
            Output 0 = aqr_exe::module::X86_WINDOW,
        )
    {
        aqr_exe::module::x86_part(
            params_len,
            res0_len,
            [in0_len, in1_len, in2_len, in3_len, in4_len, in5_len, in6_len, in7_len, in8_len, in9_len, in10_len, in11_len, in12_len],
            size,
        )
    }
}
