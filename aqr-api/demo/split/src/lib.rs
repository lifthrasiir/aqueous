//! A part module of two outputs: the bytes of `input0` at even offsets, and those at odd ones,
//! streamed through the windows in chunks.

#![cfg_attr(target_family = "wasm", no_std)]

use aqr_api::{Input, Memory, Output};

aqr_api::entry! {
    part fn split(params_len, in0_len, size0, size1) {
        assert_eq!((size0, size1), (in0_len.div_ceil(2), in0_len / 2));
        let w_in = Input::<0>::size();
        let w_out = [Output::<0>::size(), Output::<1>::size()];
        // Room in a window from a logical offset on (an output of no bytes has no window).
        let room = |w: u64, at: u64| if w == 0 { u64::MAX } else { w - at % w };
        let at = |w: u64, at: u64| if w == 0 { 0 } else { at % w };
        let (mut buf, mut outs) = ([0; 4096], [[0; 2048]; 2]);
        let mut pos = 0;
        while pos < in0_len {
            // Even but at the end, each output's bytes within its window.
            let n = (in0_len - pos).min(buf.len() as u64).min(room(w_in, pos));
            let n = n.min(room(w_out[0], pos / 2).saturating_mul(2)).min(room(w_out[1], pos / 2).saturating_mul(2));
            let chunk = &mut buf[..n as usize];
            Input::<0>::request(pos + n);
            Input::<0>::read(pos % w_in, chunk);
            Input::<0>::release(pos + n);
            let mut k = [0; 2];
            for (i, &b) in chunk.iter().enumerate() {
                let j = i % 2;
                outs[j][k[j]] = b;
                k[j] += 1;
            }
            Output::<0>::write(at(w_out[0], pos / 2), &outs[0][..k[0]]);
            Output::<0>::commit(pos / 2 + k[0] as u64);
            Output::<1>::write(at(w_out[1], pos / 2), &outs[1][..k[1]]);
            Output::<1>::commit(pos / 2 + k[1] as u64);
            aqr_api::heartbeat();
            pos += n;
        }
    }
}
