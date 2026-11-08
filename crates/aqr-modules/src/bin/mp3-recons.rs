//! `mp3-recons` (aqr-mp3), a part module: its inputs are taken and its output put in order through
//! windows, in pieces, and nothing of their lengths is held.

#![no_main]

// (its memory laid out as a stack: what it takes is its plan, `aqr_mp3::plan::decode`)
#[global_allocator]
static STACK: aqr_api::stack::Stack = aqr_api::stack::Stack::new();

aqr_api::entry! {
    part fn apply(params_len, res0_len, in0_len, in1_len, in2_len, in3_len, in4_len, in5_len, in6_len, in7_len, in8_len, in9_len, size)
        windows(
            Input 0 = aqr_mp3::module::WINDOW,
            Input 1 = aqr_mp3::module::WINDOW,
            Input 2 = aqr_mp3::module::WINDOW,
            Input 3 = aqr_mp3::module::WINDOW,
            Input 4 = aqr_mp3::module::WINDOW,
            Input 5 = aqr_mp3::module::WINDOW,
            Input 6 = aqr_mp3::module::WINDOW,
            Input 7 = aqr_mp3::module::WINDOW,
            Input 8 = aqr_mp3::module::WINDOW,
            Input 9 = aqr_mp3::module::WINDOW,
            Output 0 = aqr_mp3::module::WINDOW,
        )
    {
        aqr_mp3::module::decode_part(
            params_len,
            res0_len,
            [in0_len, in1_len, in2_len, in3_len, in4_len, in5_len, in6_len, in7_len, in8_len, in9_len],
            size,
        )
    }
}
