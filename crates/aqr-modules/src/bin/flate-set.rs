//! `flate-set` (aqr-flate), a part module: a long stream rebuilt from its data pulled a batch of
//! segments at a time, small ones in runs side by side; segments and streams decoded as tasks: 8
//! threads (the output is the same whatever the number).

#![no_main]

// (its memory laid out as a stack: what it takes is its plan, `aqr_flate::plan::plan`)
#[global_allocator]
static STACK: aqr_api::stack::Stack = aqr_api::stack::Stack::new();

#[unsafe(link_section = "aqr_task_slots")]
#[used]
static AQR_TASK_SLOTS: [u8; 1] = aqr_modules::task_slots(aqr_flate::module::SET_THREADS as u32);

aqr_api::entry! {
    part fn apply(params_len, res0_len, res1_len, in0_len, in1_len, in2_len, size)
        windows(
            Input 0 = aqr_flate::module::SET_WINDOW,
            Input 1 = aqr_flate::module::SET_RECORDS_WINDOW,
            Input 2 = aqr_flate::module::SET_RECORDS_WINDOW,
            Output 0 = aqr_flate::module::SET_WINDOW,
        )
    {
        aqr_flate::module::decode_part(params_len, res0_len, res1_len, in0_len, in1_len, in2_len, size)
    }
}
