//! `fasta-recons` (aqr-fasta), a part module.

#![no_main]

// (its memory laid out as a stack: what it takes is its plan, `aqr_fasta::module::plan`)
#[global_allocator]
static STACK: aqr_api::stack::Stack = aqr_api::stack::Stack::new();

aqr_api::entry! {
    part fn apply(params_len, in0_len, in1_len, in2_len, in3_len, in4_len, size)
        windows(
            Input 0 = aqr_fasta::module::WINDOW,
            Input 1 = aqr_fasta::module::WINDOW,
            Input 2 = aqr_fasta::module::WINDOW,
            Input 3 = aqr_fasta::module::WINDOW,
            Input 4 = aqr_fasta::module::WINDOW,
            Output 0 = aqr_fasta::module::WINDOW,
        )
    {
        aqr_fasta::module::decode_part(params_len, [in0_len, in1_len, in2_len, in3_len, in4_len], size)
    }
}
