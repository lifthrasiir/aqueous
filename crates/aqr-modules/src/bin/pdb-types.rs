//! `pdb-types` (aqr-pdb), a part module of six outputs.

#![no_main]

// (its memory laid out as a stack: what it takes is its plan, `aqr_pdb::types::plan`)
#[global_allocator]
static STACK: aqr_api::stack::Stack = aqr_api::stack::Stack::new();

aqr_api::entry! {
    part fn apply(
        params_len,
        in0_len, in1_len, in2_len, in3_len, in4_len, in5_len, in6_len, in7_len, in8_len, in9_len, in10_len, in11_len, in12_len,
        size0, size1, size2, size3, size4, size5
    )
        windows(
            Input 0 = aqr_pdb::module::TYPES_WINDOW,
            Input 1 = aqr_pdb::module::TYPES_WINDOW,
            Input 2 = aqr_pdb::module::TYPES_WINDOW,
            Input 3 = aqr_pdb::module::TYPES_WINDOW,
            Input 4 = aqr_pdb::module::TYPES_WINDOW,
            Input 5 = aqr_pdb::module::TYPES_WINDOW,
            Input 6 = aqr_pdb::module::TYPES_WINDOW,
            Input 7 = aqr_pdb::module::TYPES_WINDOW,
            Input 8 = aqr_pdb::module::TYPES_WINDOW,
            Input 9 = aqr_pdb::module::TYPES_WINDOW,
            Input 10 = aqr_pdb::module::TYPES_WINDOW,
            Input 11 = aqr_pdb::module::TYPES_WINDOW,
            Input 12 = aqr_pdb::module::TYPES_WINDOW,
            Output 0 = aqr_pdb::module::TYPES_WINDOW,
            Output 1 = aqr_pdb::module::TYPES_WINDOW,
            Output 2 = aqr_pdb::module::TYPES_WINDOW,
            Output 3 = aqr_pdb::module::TYPES_WINDOW,
            Output 4 = aqr_pdb::module::TYPES_WINDOW,
            Output 5 = aqr_pdb::module::TYPES_WINDOW,
        )
    {
        aqr_pdb::module::types_part(
            params_len,
            [in0_len, in1_len, in2_len, in3_len, in4_len, in5_len, in6_len, in7_len, in8_len, in9_len, in10_len, in11_len, in12_len],
            [size0, size1, size2, size3, size4, size5],
        )
    }
}
