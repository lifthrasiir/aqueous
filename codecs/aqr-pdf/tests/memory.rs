//! Peak heap use of coding a PDF file whose streams inflate to far more than the file: the
//! token coder holds a stream's data and tokens only a window at a time, so memory must not
//! follow them beyond the symbols the encoder keeps for its static tables. (A test binary of
//! its own: the allocator counts every thread.)

use std::io::Write;

use aqr_common::mem::{Counting, peak};
use aqr_formula::Apply;
use aqr_pdf::encode::Options;
use aqr_pdf::formula;

#[global_allocator]
static A: Counting = Counting;

fn zlib(data: &[u8]) -> Vec<u8> {
    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::new(6));
    z.write_all(data).unwrap();
    z.finish().unwrap()
}

/// A PDF of the given stream objects (dictionary body, data).
fn pdf(objs: &[(String, Vec<u8>)]) -> Vec<u8> {
    let mut f = b"%PDF-1.7\n".to_vec();
    for (i, (d, s)) in objs.iter().enumerate() {
        f.extend_from_slice(format!("{} 0 obj\n<< {d} /Length {} >>\nstream\n", i + 1, s.len()).as_bytes());
        f.extend_from_slice(s);
        f.extend_from_slice(b"\nendstream\nendobj\n");
    }
    f.extend_from_slice(b"trailer\n<< /Root 1 0 R >>\n%%EOF\n");
    f
}

#[test]
fn memory_does_not_follow_inflated_data() {
    // 48 MB of a content stream repeating a 20 KB page, and a 4000 x 2000 grey image whose
    // rows are the first rotated: long matches (the bytes are random otherwise), few tokens
    let mut x = 1u32;
    let mut random = |n: usize| -> Vec<u8> {
        (0..n)
            .map(|_| {
                x = x.wrapping_mul(1103515245).wrapping_add(12345);
                (x >> 23) as u8
            })
            .collect()
    };
    let content = random(20000).repeat(2400);
    let (w, h) = (4000usize, 2000usize);
    let row = random(w);
    let image: Vec<u8> = (0..h).flat_map(|y| row[y % w..].iter().chain(&row[..y % w]).copied()).collect();
    let file = pdf(&[
        ("/Filter /FlateDecode".into(), zlib(&content)),
        (
            format!(
                "/Type /XObject /Subtype /Image /Width {w} /Height {h} /BitsPerComponent 8 /ColorSpace /DeviceGray /Filter /FlateDecode"
            ),
            zlib(&image),
        ),
    ]);
    let n = content.len() + image.len();
    let opts = Options { speed: f64::INFINITY, full: false, png: false, ..Options::default() };
    for threads in [1, 4] {
        let opts = Options { threads, ..opts.clone() };
        let (f, enc) = peak(|| formula::encode_with(&file, &opts).expect("not coded"));
        let (back, dec) = peak(|| f.eval(&|a: &Apply, i: &[&[u8]]| formula::apply(a, i, threads)).unwrap());
        assert!(back == file, "roundtrip ({threads} threads)");
        let mb = |x: usize| x as f64 / 1e6;
        println!(
            "{threads} threads: {:.2} MB file, {:.2} MB data, peak {:.1} MB encoding, {:.1} MB decoding",
            mb(file.len()),
            mb(n),
            mb(enc),
            mb(dec)
        );
        // encoding: the symbols (4 bytes each, under one per byte of the file here) a few
        // times over as they grow and are joined, and the sections; decoding: the output, a
        // window of the data and the tables
        let f = file.len();
        let (enc_bound, dec_bound) = (10 * f + (8 << 20), 2 * f + (8 << 20));
        assert!(dec < dec_bound, "{threads} threads: decoding peak {dec} bytes, over {dec_bound}");
        assert!(enc < enc_bound, "{threads} threads: encoding peak {enc} bytes, over {enc_bound}");
    }
}
