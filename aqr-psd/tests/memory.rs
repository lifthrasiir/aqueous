//! Peak heap use of coding small PSD files: the encoder's buffers must follow the file, not
//! sit at their largest whatever its size. (A test binary of its own: the allocator counts
//! every thread.)

mod common;

use aqr_common::mem::{Counting, peak};
use aqr_psd::formula;
use common::*;

#[global_allocator]
static A: Counting = Counting;

#[test]
fn memory_follows_small_files() {
    // layers and a composite predicted from them: every part of the encoder runs
    let file = hinted_file(400, 300);
    for threads in [1, 4] {
        let (f, enc) = peak(|| formula::encode(&file, threads).expect("not coded"));
        let (back, dec) =
            peak(|| f.eval(&|a: &aqr_formula::Apply, i: &[&[u8]]| formula::apply(a, i, threads)).unwrap());
        assert!(back == file, "roundtrip ({threads} threads)");
        let mb = |x: usize| x as f64 / 1e6;
        println!(
            "{threads} threads: {:.2} MB file, peak {:.2} MB encoding, {:.2} MB decoding",
            mb(file.len()),
            mb(enc),
            mb(dec)
        );
        // the streams and rows of a few batches in flight, and the model's tables
        let bound = 4 * file.len() + (2 << 20);
        assert!(enc < bound, "{threads} threads: encoding peak {enc} bytes, over {bound}");
        assert!(dec < bound, "{threads} threads: decoding peak {dec} bytes, over {bound}");
    }
}
