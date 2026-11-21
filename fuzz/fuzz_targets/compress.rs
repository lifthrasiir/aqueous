//! A file compressed (its format recognised and recompressed: every recompressor's parsing of
//! untrusted files) and decompressed again by the native modules, which must give it back.
#![no_main]

use libfuzzer_sys::fuzz_target;

#[global_allocator]
static ALLOC: aqr_testkit::Refusing = aqr_testkit::Refusing(aqr_fuzz::ALLOCATION_MAX);

fuzz_target!(|d: &[u8]| {
    let c = aqr_compress::compress(d, &aqr_fuzz::options());
    match aqr_archive::decode(&c, &aqr_fuzz::Known) {
        Ok(back) => assert!(back == d, "decompressed as {} bytes, not the {} compressed", back.len(), d.len()),
        Err(e) => panic!("the archive made does not decompress: {e}"),
    }
});
