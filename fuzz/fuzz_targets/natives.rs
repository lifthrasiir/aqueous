//! An archive of recompressed files (stored blocks, modules by their hashes) decoded by the
//! native modules: their decoding of params and streams an archive tells.
#![no_main]

use libfuzzer_sys::fuzz_target;

#[global_allocator]
static ALLOC: aqr_testkit::Refusing = aqr_testkit::Refusing(aqr_fuzz::ALLOCATION_MAX);

fuzz_target!(|c: &[u8]| {
    let _ = aqr_archive::decode_value(c, &aqr_fuzz::Known);
});
