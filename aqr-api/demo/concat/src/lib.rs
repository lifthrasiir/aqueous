//! A whole module: `resource0 ++ input0 ++ params ++ input1`, copied between the memories directly.

#![cfg_attr(target_family = "wasm", no_std)]

use aqr_api::{CopyTo, Input, Output, Params, Resource};

fn put<M: CopyTo<0>>(at: &mut u64, len: u64) {
    assert!(len <= M::size());
    Output::<0>::copy_from::<M>(*at, 0, len);
    *at += len;
}

aqr_api::entry! {
    whole fn concat(params_len, res0_len, in0_len, in1_len, size) {
        let mut at = 0;
        put::<Resource<0>>(&mut at, res0_len);
        put::<Input<0>>(&mut at, in0_len);
        put::<Params>(&mut at, params_len);
        put::<Input<1>>(&mut at, in1_len);
        assert_eq!(at, size);
    }
}
