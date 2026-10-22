//! A part module: `input0` XOR the key in `params`, streamed through the windows in chunks.

#![cfg_attr(target_family = "wasm", no_std)]

use aqr_api::{Input, Memory, Output, Params};

aqr_api::entry! {
    part fn xor(params_len, in0_len, size) {
        if !(1..=256).contains(&params_len) {
            aqr_api::log(b"xor: key of 1 to 256 bytes expected");
            aqr_api::trap();
        }
        assert_eq!(size, in0_len);
        let mut key = [0; 256];
        let key = &mut key[..params_len as usize];
        Params::read(0, key);
        let (w_in, w_out) = (Input::<0>::size(), Output::size());
        let mut buf = [0; 4096];
        let mut pos = 0;
        while pos < size {
            let n = (size - pos).min(buf.len() as u64).min(w_in - pos % w_in).min(w_out - pos % w_out);
            let chunk = &mut buf[..n as usize];
            Input::<0>::request(pos + n);
            Input::<0>::read(pos % w_in, chunk);
            Input::<0>::release(pos + n);
            for (i, b) in chunk.iter_mut().enumerate() {
                *b ^= key[((pos + i as u64) % params_len) as usize];
            }
            Output::write(pos % w_out, chunk);
            Output::commit(pos + n);
            aqr_api::heartbeat();
            pos += n;
        }
    }
}
