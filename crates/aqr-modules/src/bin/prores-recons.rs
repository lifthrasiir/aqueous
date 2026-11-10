//! `prores-recons` (aqr-prores), a part module whose gang of tasks (one for each stripe) the
//! instance feeds a frame at a time and takes the stripes' slices from.

#![no_main]

// (its memory laid out as a stack: what it takes is its plan, `aqr_prores::module::plan`)
#[global_allocator]
static STACK: aqr_api::stack::Stack = aqr_api::stack::Stack::new();

// (one gang class: its tasks start together, and wait for each other)
#[unsafe(link_section = "aqr_task_slots")]
#[used]
static AQR_TASK_SLOTS: [u8; 1] = aqr_modules::gang_slots();

aqr_api::entry! {
    part fn apply(params_len, in0_len, in1_len, in2_len, size)
        windows(
            Input 0 = aqr_prores::module::WINDOW,
            Input 1 = aqr_prores::module::WINDOW,
            Input 2 = aqr_prores::module::WINDOW,
            Output 0 = aqr_prores::module::WINDOW,
        )
    {
        aqr_prores::module::decode_part(params_len, [in0_len, in1_len, in2_len], size)
    }
}
