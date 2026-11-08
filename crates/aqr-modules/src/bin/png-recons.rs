//! `png-recons` (aqr-png), a part module: one image at a time, its inputs taken and the file put
//! out in order.

#![no_main]

// (its memory laid out as a stack: what it takes is its plan, `aqr_png::plan::part`)
#[global_allocator]
static STACK: aqr_api::stack::Stack = aqr_api::stack::Stack::new();

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
