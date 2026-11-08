//! `flac-recons` (aqr-flac), a part module whose gang of tasks (the LMS stage and seven workers)
//! the instance feeds and takes from (the output is the same whatever the number).

#![no_main]

// (its memory laid out as a stack: what it takes is its plan, `aqr_flac::module::plan`)
#[global_allocator]
static STACK: aqr_api::stack::Stack = aqr_api::stack::Stack::new();

// (one gang class: its tasks start together, and wait for each other)
#[unsafe(link_section = "aqr_task_slots")]
#[used]
static AQR_TASK_SLOTS: [u8; 1] = aqr_modules::gang_slots();

aqr_api::entry! {
    part fn apply(params_len, res0_len, in0_len, in1_len, in2_len, in3_len, in4_len, in5_len, in6_len, in7_len, in8_len, size)
        windows(
            Input 0 = aqr_flac::module::WINDOW,
            Input 1 = aqr_flac::module::WINDOW,
            Input 2 = aqr_flac::module::WINDOW,
            Input 3 = aqr_flac::module::WINDOW,
            Input 4 = aqr_flac::module::WINDOW,
            Input 5 = aqr_flac::module::WINDOW,
            Input 6 = aqr_flac::module::WINDOW,
            Input 7 = aqr_flac::module::WINDOW,
            Input 8 = aqr_flac::module::WINDOW,
            Output 0 = aqr_flac::module::WINDOW,
        )
    {
        aqr_flac::module::decode_part(
            params_len,
            res0_len,
            [in0_len, in1_len, in2_len, in3_len, in4_len, in5_len, in6_len, in7_len, in8_len],
            size,
        )
    }
}
