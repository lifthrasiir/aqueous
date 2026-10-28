//! Peak heap use of coding a large photo-like PNG file: the data, the raw image and the
//! tokens are only ever held a window at a time, so memory must not follow them beyond the
//! symbols the encoder keeps for its static tables. (A test binary of its own: the
//! allocator counts every thread.)

mod common;

use aqr_common::mem::{Counting, peak};
use common::*;

#[global_allocator]
static A: Counting = Counting;

#[test]
fn memory_does_not_follow_data() {
    // 2048 x 1536 RGB: 9.4 MB of image data, mostly literals under zlib's level 6
    let (w, h) = (2048, 1536);
    let data = image_data(w, h, 8, 2, false, 7);
    let n = data.len();
    let mut cs = vec![(b"IHDR", ihdr(w as u32, h as u32, 8, 2, 0))];
    cs.extend(idats(&zlib(&zlib6(), &data), 1 << 16));
    cs.push((b"IEND", vec![]));
    let file = png(&cs);
    for threads in [1, 4] {
        let (f, enc) = peak(|| aqr_png::formula::encode(&file, threads).expect("not coded"));
        let (back, dec) = peak(|| f.eval(&modules(threads)).unwrap());
        assert!(back == file, "roundtrip ({threads} threads)");
        let mb = |x: usize| x as f64 / 1e6;
        println!(
            "{threads} threads: {:.2} MB file, {:.2} MB data, peak {:.1} MB encoding, {:.1} MB decoding",
            mb(file.len()),
            mb(n),
            mb(enc),
            mb(dec)
        );
        // encoding: the symbols (2 bytes each, about one per byte) and the file's size a few
        // times over (payload, rANS stream, records); decoding: the payload and the file,
        // and windows of the data
        let f = file.len();
        let (enc_bound, dec_bound) = (2 * n + 3 * f + (8 << 20), 2 * f + (8 << 20));
        assert!(enc < enc_bound, "{threads} threads: encoding peak {enc} bytes, over {enc_bound}");
        assert!(dec < dec_bound, "{threads} threads: decoding peak {dec} bytes, over {dec_bound}");
    }
}
