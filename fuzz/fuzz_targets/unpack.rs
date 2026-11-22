//! An archive of another format read by aqr-unpack as `aqr -d` reads it (`src/unpack.rs`): its
//! listing, its decryption, its decoding, and of some paths alone, alike.
#![no_main]

use libfuzzer_sys::fuzz_target;

#[global_allocator]
static ALLOC: aqr_testkit::Refusing = aqr_testkit::Refusing(aqr_fuzz::ALLOCATION_MAX);

fuzz_target!(|d: &[u8]| aqr_fuzz::unpack::unpack(d));
