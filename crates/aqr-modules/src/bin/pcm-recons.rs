//! `pcm-recons` (aqr-raw), a part module whose gang of tasks (the LMS stage and seven workers) the
//! instance feeds and takes from (the output is the same whatever the number).

#![no_main]

// (its memory laid out as a stack: what it takes is its plan, `aqr_raw::module::pcm::plan`)
#[global_allocator]
static STACK: aqr_api::stack::Stack = aqr_api::stack::Stack::new();

// (one gang class: its tasks start together, and wait for each other)
#[unsafe(link_section = "aqr_task_slots")]
#[used]
static AQR_TASK_SLOTS: [u8; 1] = aqr_modules::gang_slots();

aqr_api::entry! {
    part fn apply(params_len, res0_len, in0_len, in1_len, in2_len, in3_len, in4_len, in5_len, size)
        windows(
            Input 0 = aqr_raw::module::pcm::WINDOW,
            Input 1 = aqr_raw::module::pcm::WINDOW,
            Input 2 = aqr_raw::module::pcm::WINDOW,
            Input 3 = aqr_raw::module::pcm::WINDOW,
            Input 4 = aqr_raw::module::pcm::WINDOW,
            Input 5 = aqr_raw::module::pcm::WINDOW,
            Output 0 = aqr_raw::module::pcm::WINDOW,
        )
    {
        aqr_raw::module::pcm::decode_part(
            params_len,
            res0_len,
            [in0_len, in1_len, in2_len, in3_len, in4_len, in5_len],
            size,
        )
    }
}
