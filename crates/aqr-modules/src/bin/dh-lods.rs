//! `dh-lods` (aqr-minecraft), a part module: Distant Horizons' blobs' contents put out in order,
//! the parents' made of their children's quadrants.

#![no_main]

// (its memory laid out as a stack: what it takes is its plan, `aqr_minecraft::lods::plan`)
#[global_allocator]
static STACK: aqr_api::stack::Stack = aqr_api::stack::Stack::new();

aqr_api::entry! {
    part fn apply(params_len, in0_len, in1_len, in2_len, size)
        windows(
            Input 0 = aqr_minecraft::lods::SMALL_WINDOW,
            Input 1 = aqr_minecraft::lods::WINDOW,
            Input 2 = aqr_minecraft::lods::SMALL_WINDOW,
            Output 0 = aqr_minecraft::lods::WINDOW,
        )
    {
        aqr_minecraft::lods::decode_part(params_len, in0_len, in1_len, in2_len, size)
    }
}
