//! Peak heap use of coding a long MP3 file: the rANS symbols are held a run of frames at a
//! time, the decoder writes the output as the reservoir is rebuilt, and both share the built-in
//! tables, so memory follows the file only by a few times its size. (A test binary of its own:
//! the allocator counts every thread.)

use aqr_common::mem::{self, Counting};
use aqr_formula::Apply;
use aqr_mp3::formula::{self, MODULE};

#[global_allocator]
static A: Counting = Counting;

fn module(a: &Apply, inputs: &[&[u8]]) -> aqr_formula::Result<Vec<u8>> {
    assert_eq!(a.module, MODULE);
    let res: Vec<&[u8]> = a.resources.iter().map(|r| &**r).collect();
    formula::decode(&a.params, &res, inputs, a.len, 1).map_err(|e| aqr_formula::Error::new(e.0))
}

#[test]
fn memory_follows_file_little() {
    let one = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/cbr128.mp3")).unwrap();
    let data: Vec<u8> = one.iter().copied().cycle().take(16 << 20).collect();
    let (f, enc) = mem::peak(|| formula::encode(&data, 1).expect("not coded"));
    let (back, dec) = mem::peak(|| f.eval(&module).unwrap());
    assert!(back == data, "roundtrip");
    let mb = |x: usize| x as f64 / 1e6;
    println!("{:.2} MB file, peak {:.1} MB encoding, {:.1} MB decoding", mb(data.len()), mb(enc), mb(dec));
    // the tables, of which the encoder's inverse maps (54 MB) are allocated whole but only
    // touched for the contexts used
    let (enc_bound, dec_bound) = (4 * data.len() + (96 << 20), 3 * data.len() + (32 << 20));
    assert!(enc < enc_bound, "peak {enc} bytes encoding, over {enc_bound}");
    assert!(dec < dec_bound, "peak {dec} bytes decoding, over {dec_bound}");
}
