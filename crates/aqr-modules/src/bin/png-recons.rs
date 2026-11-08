//! `png-recons` (aqr-png), a part module: its inputs taken and the file put out in order, an image
//! at a time, or a batch of small ones decoded as tasks (the output is the same whatever the number).

#![no_main]

// (its memory laid out as a stack: what it takes is its plan, `aqr_png::plan::part`)
#[global_allocator]
static STACK: aqr_api::stack::Stack = aqr_api::stack::Stack::new();

#[unsafe(link_section = "aqr_task_slots")]
#[used]
static AQR_TASK_SLOTS: [u8; 1] = aqr_modules::task_slots(aqr_png::part::THREADS as u32);

aqr_api::entry! {
    part fn apply(params_len, in0_len, in1_len, in2_len, in3_len, in4_len, size)
        windows(
            Input 0 = aqr_png::part::WINDOW,
            Input 1 = aqr_png::part::WINDOW,
            Input 2 = aqr_png::part::WINDOW,
            Input 3 = aqr_png::part::WINDOW,
            Input 4 = aqr_png::part::WINDOW,
            Output 0 = aqr_png::part::WINDOW,
        )
    {
        aqr_png::part::decode_part(params_len, [in0_len, in1_len, in2_len, in3_len, in4_len], size)
    }
}
